use anyhow::{Context, ensure};
use std::env;

/// 进程配置由环境变量注入，各实例必须共享认证及模型配置。
#[derive(Clone)]
pub struct Config {
    /// PostgreSQL 连接串。
    pub database_url: String,
    /// 管理员根密钥，至少 32 字节。
    pub admin_token: String,
    /// 浏览器唯一可信源，也是生成链接的基础地址。
    pub public_url: String,
    /// API 的公开源，用于 OAuth 回调及 API 域名上的安全 Cookie。
    pub api_public_url: String,
    /// Railway 注入的监听端口。
    pub port: u16,
    /// 每实例并行任务数量。
    pub workers: usize,
    /// OpenAI 兼容模型参数。
    pub model: agent_runtime::ModelConfig,
    /// 可选飞书接入；缺少配置时拒绝启用。
    pub feishu: Option<FeishuConfig>,
    /// 文件记忆与可选混合检索配置。
    pub memory: Option<crate::memory::config::MemoryConfig>,
    /// 新身份的默认时区，之后可按身份修改。
    pub followup_timezone: String,
    /// 用户授权的沟通资料采集，与机器人消息入口独立启用。
    pub communications: Option<crate::communications::Config>,
    /// 独立的 Linear 个人 API Key 配置。
    pub linear: Option<crate::linear::Config>,
    /// 可接管问题的独立 JSON 文件路径，相对路径以进程工作目录为基准。
    pub takeover_questions_file: std::path::PathBuf,
    /// 可选 Jev 判断服务，仅用于个人消息接管。
    pub typesafe: Option<crate::communications::takeover::Config>,
}

/// 飞书自建应用配置，白名单限制个人服务的访问者。
#[derive(Clone)]
pub struct FeishuConfig {
    /// 应用 ID。
    pub app_id: String,
    /// 应用密钥，仅用于获取 tenant_access_token。
    pub app_secret: String,
    /// 回调体的校验令牌。
    pub verification_token: String,
    /// 签名和解密使用的密钥，启用飞书时必须设置。
    pub encrypt_key: String,
    /// 获准使用机器人的用户 open_id。
    pub allowed_users: Vec<String>,
    /// 官方 API 根地址；测试可替换为本地协议夹具。
    pub api_base: String,
}

impl Config {
    /// 在监听端口前验证必要配置，避免部署成功后才发现不可用。
    pub fn from_env() -> anyhow::Result<Self> {
        let admin_token = required("ADMIN_TOKEN")?;
        ensure!(admin_token.len() >= 32, "ADMIN_TOKEN 至少需要 32 字节");
        let public_url = public_origin("PUBLIC_URL", &required("PUBLIC_URL")?)?;
        let api_public_url = public_origin("API_PUBLIC_URL", &required("API_PUBLIC_URL")?)?;
        let base_url = required("OPENAI_BASE_URL")?;
        let upstream = reqwest::Url::parse(&base_url).context("OPENAI_BASE_URL 格式错误")?;
        ensure!(
            matches!(upstream.scheme(), "https" | "http")
                && upstream.host_str().is_some()
                && upstream.username().is_empty()
                && upstream.password().is_none()
                && upstream.query().is_none()
                && upstream.fragment().is_none(),
            "模型地址必须是 HTTP(S) API 根路径，不能包含凭证、查询或片段"
        );
        let workers = env::var("WORKER_CONCURRENCY")
            .unwrap_or("2".into())
            .parse::<usize>()?;
        ensure!(
            (1..=16).contains(&workers),
            "WORKER_CONCURRENCY 范围是 1–16"
        );
        let followup_timezone = env::var("FOLLOWUP_TIMEZONE").unwrap_or("Asia/Shanghai".into());
        ensure!(
            followup_timezone.parse::<chrono_tz::Tz>().is_ok(),
            "FOLLOWUP_TIMEZONE 必须是 IANA 时区"
        );
        let feishu = if env::var("FEISHU_APP_ID").is_ok_and(|v| !v.is_empty()) {
            let allowed_users: Vec<String> = required("FEISHU_ALLOWED_USERS")?
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .collect();
            ensure!(!allowed_users.is_empty(), "飞书必须配置用户白名单");
            Some(FeishuConfig {
                api_base: "https://open.feishu.cn/open-apis".into(),
                app_id: required("FEISHU_APP_ID")?,
                app_secret: required("FEISHU_APP_SECRET")?,
                verification_token: required("FEISHU_VERIFICATION_TOKEN")?,
                encrypt_key: required("FEISHU_ENCRYPT_KEY")?,
                allowed_users,
            })
        } else {
            None
        };
        let memory = crate::memory::config::MemoryConfig::from_env()?;
        let communications = crate::communications::Config::from_env()?;
        ensure!(
            communications.is_none() || (feishu.is_some() && memory.is_some()),
            "沟通采集需要飞书配置及本地记忆存储"
        );
        Ok(Self {
            linear: crate::linear::Config::from_env()?,
            typesafe: crate::communications::takeover::Config::from_env()?,
            takeover_questions_file: env::var("TAKEOVER_QUESTIONS_FILE")
                .unwrap_or_else(|_| "config/takeover-questions.json".into())
                .into(),
            communications,
            followup_timezone,
            database_url: required("DATABASE_URL")?,
            admin_token,
            public_url,
            api_public_url,
            port: env::var("PORT").unwrap_or("8080".into()).parse()?,
            workers,
            model: agent_runtime::ModelConfig {
                base_url,
                model: required("OPENAI_MODEL")?,
                api_key: required("OPENAI_API_KEY")?,
                chat_output_tokens: env::var("AGENT_CHAT_OUTPUT_TOKENS")
                    .unwrap_or_else(|_| agent_runtime::DEFAULT_CHAT_OUTPUT_TOKENS.to_string())
                    .parse()
                    .context("AGENT_CHAT_OUTPUT_TOKENS 必须是整数")?,
                stream_max_bytes: env::var("AGENT_STREAM_MAX_BYTES")
                    .unwrap_or_else(|_| agent_runtime::DEFAULT_STREAM_MAX_BYTES.to_string())
                    .parse()
                    .context("AGENT_STREAM_MAX_BYTES 必须是整数")?,
                stream_enabled: env::var("AGENT_STREAM_ENABLED")
                    .unwrap_or("true".into())
                    .parse()?,
                tools_enabled: env::var("AGENT_TOOLS_ENABLED")
                    .unwrap_or("true".into())
                    .parse()?,
            },
            feishu,
            memory,
        })
    }
}

/// 两个服务只接受规范化的完整源，拒绝路径和凭证，避免回调或来源校验歧义。
fn public_origin(key: &str, value: &str) -> anyhow::Result<String> {
    let origin = value.trim_end_matches('/');
    let url = reqwest::Url::parse(origin).with_context(|| format!("{key} 格式错误"))?;
    ensure!(
        url.origin().ascii_serialization() == origin,
        "{key} 必须是无路径的完整源地址"
    );
    ensure!(
        url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("localhost" | "127.0.0.1"))),
        "{key} 仅本地允许 HTTP"
    );
    Ok(origin.to_owned())
}

/// 不回显变量值，确保启动错误不会输出密钥。
fn required(key: &str) -> anyhow::Result<String> {
    env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .with_context(|| format!("缺少环境变量 {key}"))
}
