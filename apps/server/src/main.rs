use orbit_server::{AppState, config::Config, feishu, followups, memory, router, worker};
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;
use tokio::{sync::watch, task::JoinSet};
use tracing_subscriber::EnvFilter;

// 对话之外还有调度、发现、出站、记忆和健康检查；事务检查会短暂占用第二条连接。
const BACKGROUND_CONNECTIONS: usize = 12;
// 每个数据库 schema 仅允许一个文件记忆写入者。
const MEMORY_WRITER_LOCK_KEY: i32 = 72_401;

/// 启动顺序为配置校验、数据库迁移、worker 和 HTTP；任何关键后台任务退出都终止实例。
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("orbit_server=info,agent_runtime=info,tower_http=warn")
        }))
        .init();
    let config = Config::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections((config.workers + BACKGROUND_CONNECTIONS) as u32)
        .acquire_timeout(Duration::from_secs(3))
        .after_connect(|connection, _| {
            Box::pin(async move {
                // 数据库请求也需要上限，避免锁竞争耗尽连接池和 worker。
                sqlx::query("SET statement_timeout = '10s'")
                    .execute(&mut *connection)
                    .await?;
                sqlx::query("SET lock_timeout = '3s'")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect(&config.database_url)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.port)).await?;
    let state = AppState::new(config, pool)?;
    let _memory_directory_lock = state
        .config
        .memory
        .as_ref()
        .map(memory::lock_directory)
        .transpose()?;
    // 文件是主存储，拒绝第二个应用实例写同一数据库的记忆。
    let mut memory_connection = if state.config.memory.is_some() {
        let mut connection = state.pool.acquire().await?;
        let acquired: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtext(current_schema()),$1)")
                .bind(MEMORY_WRITER_LOCK_KEY)
                .fetch_one(&mut *connection)
                .await?;
        anyhow::ensure!(acquired, "文件记忆仅支持单实例；已有实例持有写入锁");
        memory::initialize(&state).await?;
        Some(connection)
    } else {
        None
    };
    orbit_server::communications::sync::initialize_scope(&state)
        .await
        .map_err(|error| anyhow::anyhow!(error.1))?;
    let (stop, receiver) = watch::channel(false);
    let mut workers = JoinSet::new();
    for _ in 0..state.config.workers {
        workers.spawn(worker::run_worker(state.clone(), receiver.clone()));
    }
    workers.spawn(followups::scheduler::run(
        state.clone(),
        "reminder",
        receiver.clone(),
    ));
    workers.spawn(followups::scheduler::run(
        state.clone(),
        "checkin",
        receiver.clone(),
    ));
    workers.spawn(orbit_server::todos::scheduler::run(
        state.clone(),
        receiver.clone(),
    ));
    workers.spawn(followups::discovery::run(state.clone(), receiver.clone()));
    workers.spawn(memory::worker::run(state.clone(), receiver.clone()));
    workers.spawn(orbit_server::communications::sync::run(
        state.clone(),
        receiver.clone(),
    ));
    workers.spawn(feishu::delivery_worker(state.clone(), receiver.clone()));
    let mut http_stop = receiver.clone();
    let app = router(state.clone());
    let mut server = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = http_stop.changed().await;
            })
            .await
    });
    tracing::info!(
        port = state.config.port,
        workers = state.config.workers,
        "Orbit 已启动"
    );
    tokio::select! {
        // 主函数一直持有锁连接，优雅退出期间也不允许新实例并发写原文。
        _ = async {
            if let Some(connection) = memory_connection.as_mut() {
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    if sqlx::query("SELECT 1").execute(&mut **connection).await.is_err() { break; }
                }
            } else { std::future::pending::<()>().await; }
        } => {
            tracing::error!("记忆单写者锁连接断开，终止实例");
            std::process::exit(1);
        }
        _ = shutdown_signal() => {
            tracing::info!("收到退出信号，停止领取新任务");
        }
        result = workers.join_next() => {
            tracing::error!(?result, "关键 worker 意外退出");
            std::process::exit(1);
        }
        result = &mut server => {
            result??;
            anyhow::bail!("HTTP 服务意外退出");
        }
    }
    stop.send(true)?;
    let drain = async {
        server.await??;
        while let Some(result) = workers.join_next().await {
            result?;
        }
        Ok::<_, anyhow::Error>(())
    };
    match tokio::time::timeout(Duration::from_secs(130), drain).await {
        Ok(result) => result?,
        Err(_) => {
            workers.abort_all();
            tracing::warn!("优雅退出超时，未完成任务将由租约恢复");
        }
    }
    drop(memory_connection);
    state.pool.close().await;
    Ok(())
}

/// Railway 发送 SIGTERM，本地开发通常使用 Ctrl-C。
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("注册 SIGTERM 失败");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
