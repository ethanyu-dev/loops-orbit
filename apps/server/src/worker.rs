use crate::{AppState, error::ApiResult};
use sqlx::FromRow;
use std::time::Duration;
use tokio::sync::watch;
use tracing::Instrument;
use uuid::Uuid;

// 总执行时间小于租约时间；即使旧 worker 卡顿，提交时仍需匹配租约令牌。
const RUN_TIMEOUT_SECONDS: u64 = 120;
const MAX_ATTEMPTS: i32 = 3;
// 轮询取消并批量保存生成快照，避免每个 token 写一次数据库。
const PROGRESS_INTERVAL_MS: u64 = 200;

// 任务领取 SQL 单独排版，保持租约和会话串行规则可审阅。
const CLAIM_RUN_SQL: &str = include_str!("sql/claim_run.sql");

/// 数据库领取的任务快照，不输出输入内容到日志。
#[derive(FromRow)]
pub struct Job {
    /// 任务 ID，同时用于追踪日志。
    pub id: Uuid,
    /// 任务所属会话。
    pub conversation_id: Uuid,
    /// 会话内排序边界，排除之后才提交的输入。
    pub seq: i64,
    /// 连续输入所属批次，取消前不能压缩成稳定摘要。
    pub batch_id: Uuid,
    /// 当前用户输入。
    pub input: String,
    /// 已领取的次数，用于限制故障重试。
    pub attempts: i32,
    /// 本次领取专属令牌，防止过期执行器写回。
    pub lease_token: Option<Uuid>,
    /// 飞书原消息 ID，网页任务为空。
    pub reply_to: Option<String>,
}

/// 用单条 SQL 原子领取任务，同会话严格串行、不同会话可跨实例并发。
pub async fn claim(state: &AppState) -> ApiResult<Option<Job>> {
    Ok(sqlx::query_as(CLAIM_RUN_SQL)
        .bind(Uuid::new_v4())
        .fetch_optional(&state.pool)
        .await?)
}

/// 循环领取持久化任务，收到关闭信号后完成当前任务并停止领取。
pub async fn run_worker(state: AppState, mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            break;
        }
        match claim(&state).await {
            Ok(Some(job)) => {
                let span = tracing::info_span!(
                    "agent_run",
                    run_id = %job.id,
                    conversation_id = %job.conversation_id,
                    attempt = job.attempts,
                );
                if process(&state, &job).instrument(span).await.is_err() {
                    tracing::error!(run_id = %job.id, "任务写回失败，等待租约恢复");
                }
            }
            Ok(None) => {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {},
                    _ = stop.changed() => {},
                }
            }
            Err(_) => {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {},
                    _ = stop.changed() => {},
                }
            }
        }
    }
}

/// 生成与取消检测同时运行，撤销租约后立即丢弃本地模型请求。
async fn generate(
    state: &AppState,
    job: &Job,
) -> ApiResult<Option<Result<String, agent_runtime::Failure>>> {
    let (progress, latest) = watch::channel(String::new());
    let work = async {
        let owner: String = sqlx::query_scalar("SELECT owner FROM conversations WHERE id=$1")
            .bind(job.conversation_id)
            .fetch_one(&state.pool)
            .await
            .map_err(|_| agent_runtime::Failure {
                code: "storage_unavailable",
                retryable: true,
            })?;
        let external = !crate::auth::is_account_owner(state, &owner)
            .await
            .map_err(|error| agent_runtime::Failure {
                code: error.1,
                retryable: true,
            })?;
        // 读取已发布知识并记录知识代次，使撤回取消在途生成并清空旧历史。
        let retrieved =
            crate::rag::retrieve(state, &job.input, (!external).then_some(owner.as_str()))
                .await
                .map_err(|error| agent_runtime::Failure {
                    code: error.1,
                    retryable: true,
                })?;
        let knowledge = crate::knowledge::published(&retrieved);
        {
            let _guard = state.communications.lock().await;
            for entry in &knowledge {
                if !crate::knowledge::current(state, entry.id, entry.version)
                    .await
                    .map_err(|error| agent_runtime::Failure {
                        code: error.1,
                        retryable: true,
                    })?
                {
                    return Err(agent_runtime::Failure {
                        code: "knowledge_changed",
                        retryable: true,
                    });
                }
            }
            sqlx::query("UPDATE runs SET knowledge_revision=(SELECT revision FROM knowledge_state) WHERE id=$1 AND lease_token=$2 AND status='running'")
            .bind(job.id).bind(job.lease_token).execute(&state.pool).await
            .map_err(|_|agent_runtime::Failure{code:"storage_unavailable",retryable:true})?;
        }
        let mut history = crate::context::prepare_for_audience(state, job, external)
            .await
            .map_err(|error| agent_runtime::Failure {
                code: error.1,
                retryable: error.0.is_server_error(),
            })?;
        if let Some(content) = crate::knowledge::context(&knowledge) {
            history.insert(
                0,
                agent_runtime::Message {
                    role: "user".into(),
                    content,
                },
            );
        }
        if !external && state.config.memory.is_some() {
            let background = crate::memory::context(state, &owner, &history)
                .await
                .map_err(|error| agent_runtime::Failure {
                    code: error.1,
                    retryable: true,
                })?;
            if !external && let Some(communications) = crate::rag::private_context(&retrieved) {
                history.insert(
                    0,
                    agent_runtime::Message {
                        role: "user".into(),
                        content: communications,
                    },
                );
            }
            history.insert(
                0,
                agent_runtime::Message {
                    role: "user".into(),
                    content: background,
                },
            );
        }
        sqlx::query("UPDATE runs SET phase='responding' WHERE id=$1 AND lease_token=$2 AND status='running'")
            .bind(job.id).bind(job.lease_token).execute(&state.pool).await
            .map_err(|_| agent_runtime::Failure { code: "storage_unavailable", retryable: true })?;
        let host =
            crate::tools::Host::new(state, job)
                .await
                .map_err(|error| agent_runtime::Failure {
                    code: error.1,
                    retryable: true,
                })?;
        if !external
            && let Some(background) =
                host.background()
                    .await
                    .map_err(|error| agent_runtime::Failure {
                        code: error.1,
                        retryable: true,
                    })?
        {
            history.insert(
                0,
                agent_runtime::Message {
                    role: "user".into(),
                    content: background,
                },
            );
        }
        state
            .runtime
            .run_for_audience(
                &history,
                Some(&progress),
                (state.config.model.tools_enabled && !external)
                    .then_some(&host as &dyn agent_runtime::tools::Host),
                external,
            )
            .await
    };
    let work = tokio::time::timeout(Duration::from_secs(RUN_TIMEOUT_SECONDS), work);
    tokio::pin!(work);
    let mut tick = tokio::time::interval(Duration::from_millis(PROGRESS_INTERVAL_MS));
    let mut saved = String::new();
    loop {
        tokio::select! {
            result = &mut work => return Ok(Some(result.unwrap_or(Err(agent_runtime::Failure {
                code: "run_timeout", retryable: true,
            })))),
            _ = tick.tick() => {
                let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM runs WHERE id=$1 AND lease_token=$2 AND status='running')")
                    .bind(job.id).bind(job.lease_token).fetch_one(&state.pool).await?;
                if !active { return Ok(None); }
                let text = latest.borrow().clone();
                if text != saved {
                    sqlx::query("UPDATE runs SET partial_content=$3 WHERE id=$1 AND lease_token=$2 AND status='running'")
                        .bind(job.id).bind(job.lease_token).bind(&text).execute(&state.pool).await?;
                    saved = text;
                }
            }
        }
    }
}

/// 成功结果与回复队列在同一事务内提交；暂时故障指数退避，永久错误直接结束。
async fn process(state: &AppState, job: &Job) -> ApiResult<()> {
    let started = std::time::Instant::now();
    // 进程中断后可能已提交工具结果；此时不重新运行用户动作。
    let saved: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM todo_operations WHERE run_id=$1)")
            .bind(job.id)
            .fetch_one(&state.pool)
            .await?;
    let result = if saved {
        Err(agent_runtime::Failure {
            code: "reply_interrupted_after_save",
            retryable: false,
        })
    } else if job.attempts > MAX_ATTEMPTS {
        Err(agent_runtime::Failure {
            code: "attempt_limit",
            retryable: false,
        })
    } else {
        let Some(result) = generate(state, job).await? else {
            return Ok(());
        };
        result
    };
    match result {
        Ok(answer) => {
            let mut tx = state.pool.begin().await?;
            // 与发送和取消共用会话锁，让完成与改口具备明确的先后顺序。
            sqlx::query("SELECT id FROM conversations WHERE id=$1 FOR UPDATE")
                .bind(job.conversation_id)
                .execute(&mut *tx)
                .await?;
            let updated = sqlx::query(
                r#"
                UPDATE runs
                SET status = 'completed', error = NULL, finished_at = now(), lease_until = NULL, partial_content = '', phase = 'completed'
                WHERE id = $1 AND lease_token = $2 AND status = 'running'
            "#,
            )
            .bind(job.id)
            .bind(job.lease_token)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() == 0 {
                return Ok(());
            }
            sqlx::query(
                "INSERT INTO messages(conversation_id, run_id, role, content) \
                 VALUES($1, $2, 'assistant', $3)",
            )
            .bind(job.conversation_id)
            .bind(job.id)
            .bind(&answer)
            .execute(&mut *tx)
            .await?;
            if let Some(reply_to) = &job.reply_to {
                enqueue_reply(&mut tx, job.id, reply_to, &answer).await?;
            }
            if state
                .config
                .memory
                .as_ref()
                .is_some_and(|config| config.auto_extract)
            {
                sqlx::query("INSERT INTO memory_jobs(run_id,owner,source_seq) SELECT $1,owner,$2 FROM conversations WHERE id=$3 ON CONFLICT DO NOTHING")
                    .bind(job.id).bind(job.seq).bind(job.conversation_id).execute(&mut *tx).await?;
            }
            sqlx::query("INSERT INTO followup_discovery(run_id,owner) SELECT $1,c.owner FROM conversations c JOIN followup_preferences p ON p.owner=personal_owner(c.owner) AND p.enabled WHERE c.id=$2 ON CONFLICT DO NOTHING")
                .bind(job.id).bind(job.conversation_id).execute(&mut *tx).await?;
            tx.commit().await?;
            tracing::info!(
                duration_ms = started.elapsed().as_millis() as u64,
                "任务完成"
            );
        }
        Err(error) => {
            let mut tx = state.pool.begin().await?;
            // 与发送和取消共用会话锁，让完成与改口具备明确的先后顺序。
            sqlx::query("SELECT id FROM conversations WHERE id=$1 FOR UPDATE")
                .bind(job.conversation_id)
                .execute(&mut *tx)
                .await?;
            let saved_reply = crate::todos::receipts::saved_reply(&mut tx, job.id).await?;
            let retry = saved_reply.is_none() && error.retryable && job.attempts < MAX_ATTEMPTS;
            let updated = sqlx::query(
                r#"
                UPDATE runs
                SET status = $3,
                    error = $4,
                    partial_content = '',
                    phase = CASE WHEN $6 THEN 'saved_reply_failed' ELSE $3 END,
                    lease_until = NULL,
                    available_at = now() + make_interval(secs => $5),
                    finished_at = CASE WHEN $3 = 'failed' THEN now() ELSE NULL END
                WHERE id = $1 AND lease_token = $2 AND status = 'running'
            "#,
            )
            .bind(job.id)
            .bind(job.lease_token)
            .bind(if retry { "queued" } else { "failed" })
            .bind(error.code)
            .bind(f64::from(2_i32.pow(job.attempts.min(5) as u32)))
            .bind(saved_reply.is_some())
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() > 0
                && let Some(reply) = &saved_reply
            {
                // 网页与飞书读取同一份确定性回执；失败码仍保留在运行记录中。
                sqlx::query("INSERT INTO messages(conversation_id,run_id,role,content) VALUES($1,$2,'assistant',$3) ON CONFLICT DO NOTHING")
                    .bind(job.conversation_id).bind(job.id).bind(reply).execute(&mut *tx).await?;
            }
            if !retry
                && updated.rows_affected() > 0
                && let Some(reply_to) = &job.reply_to
            {
                enqueue_reply(
                    &mut tx,
                    job.id,
                    reply_to,
                    saved_reply.as_deref().unwrap_or(match error.code {
                        "provider_output_limit" => "模型输出达到上限，这次请求未能完成。请缩小问题范围后重试，或联系管理员调整输出预算。",
                        "provider_content_filtered" => "模型服务过滤了本次输出，请调整问题后重试。",
                        _ => "这次请求暂时无法完成，请稍后重试。管理员可以在 Orbit 控制台查看失败状态。",
                    }),
                )
                .await?;
            }
            tx.commit().await?;
            tracing::warn!(
                code = error.code,
                retry,
                duration_ms = started.elapsed().as_millis() as u64,
                "任务执行失败"
            );
        }
    }
    Ok(())
}

/// 回复使用任务 ID 作为幂等标识，避免队列重试生成新消息身份。
async fn enqueue_reply(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    reply_to: &str,
    content: &str,
) -> ApiResult<()> {
    // 队列保留原有展示长度；最终卡片/富文本的序列化字节限制由投递层检查。
    let mut text: String = content.chars().take(6000).collect();
    if content.chars().count() > 6000 {
        text.push_str("\n\n（内容较长，完整回复请在网页查看。）");
    }
    sqlx::query(
        "INSERT INTO outbox(id,reply_to,content) VALUES($1,$2,$3) ON CONFLICT(id) DO NOTHING",
    )
    .bind(id)
    .bind(reply_to)
    .bind(text)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
