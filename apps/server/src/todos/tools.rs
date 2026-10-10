use super::{Create, Source, Update, identity, service};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
    worker::Job,
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};
use uuid::Uuid;

// 渠道名称必须作为投递动作的目标出现，不能把“汇总飞书资料”当作切换渠道。
const CHANNEL_TARGET_PREFIXES: &[&str] = &[
    "推送到",
    "推送至",
    "发送到",
    "发送至",
    "发到",
    "送到",
    "投递到",
    "改到",
    "改成",
    "改为",
    "推送的是",
    "发送的是",
    "渠道是",
    "渠道为",
    "渠道设为",
    "渠道设置为",
    "sendto",
    "deliverto",
    "switchto",
    "notifyvia",
];
// 保守拒绝含否定的整句；模型必须澄清，不能截短引文绕过。
const CHANNEL_NEGATIONS: &[&str] = &["不", "别", "取消", "禁止", "not", "never", "don't"];

/// 对话待办工具固定真实入口身份，模型不能替换数据主体或运行租约。
pub struct Provider<'a> {
    /// 共享存储与权限校验入口。
    state: &'a AppState,
    /// 当前真实聊天任务，写入时重新核验租约。
    job: &'a Job,
    /// 原始渠道身份，由宿主注入。
    owner: String,
    /// 本轮用户原文，用于验证显式修改依据。
    inputs: Vec<String>,
    /// 创建宿主时的绑定版本。
    version: Option<i64>,
    /// 当前会话的可信渠道，只用于提示模型；默认值仍由服务端解析。
    channel: String,
}
impl<'a> Provider<'a> {
    /// 仅本人注册待办工具；飞书白名单中的其他用户不会继承。
    pub async fn new(
        state: &'a AppState,
        job: &'a Job,
        owner: &str,
        inputs: Vec<String>,
    ) -> ApiResult<Option<Self>> {
        if identity::principal(state, owner).await? != "admin" {
            return Ok(None);
        }
        Ok(Some(Self {
            state,
            job,
            owner: owner.into(),
            inputs,
            version: identity::version(state, owner).await?,
            channel: sqlx::query_scalar("SELECT channel FROM conversations WHERE id=$1")
                .bind(job.conversation_id)
                .fetch_one(&state.pool)
                .await?,
        }))
    }
    /// 最近事项只作为有界背景，不自动把本轮会话永久绑定到一个猜测的目标。
    pub async fn background(&self) -> ApiResult<Option<String>> {
        let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',id,'title',title,'status',status,'next_action',next_action,'waiting_on',waiting_on,'version',version) FROM todos WHERE owner='admin' AND status NOT IN('completed','cancelled') ORDER BY updated_at DESC LIMIT 12")
            .fetch_all(&self.state.pool).await?;
        Ok((!rows.is_empty()).then(||json!({"background_type":"personal_todos","items":rows,"note":"数据背景；名称或指代不明确时先查询/澄清，使用工具才可修改。"}).to_string()))
    }
    async fn call(&self, name: &str, mut args: Value) -> ApiResult<Value> {
        let active: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=$1 AND status='running' AND lease_token=$2)",
        )
        .bind(self.job.id)
        .bind(self.job.lease_token)
        .fetch_one(&self.state.pool)
        .await?;
        if !active || identity::version(self.state, &self.owner).await? != self.version {
            return Err(ApiError(StatusCode::CONFLICT, "run_superseded"));
        }
        if name == "todo_search" {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Search {
                #[serde(default)]
                query: String,
                #[serde(default)]
                offset: i64,
            }
            let q: Search = serde_json::from_value(args)
                .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"))?;
            return service::list(self.state, &self.owner, &q.query, q.offset, "all").await;
        }
        if name == "todo_get" {
            if args.as_object().is_none_or(|a| a.len() != 1) {
                return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"));
            }
            return service::detail(self.state, &self.owner, parse_id(&args)?).await;
        }
        let evidence = args["evidence"].as_str().unwrap_or("");
        if evidence.chars().count() < 2 || !self.inputs.iter().any(|s| s.contains(evidence)) {
            return Err(ApiError(StatusCode::BAD_REQUEST, "todo_evidence_required"));
        }
        // 显式渠道必须有独立正向原文依据，防止模型把猜测当作默认值。
        if matches!(name, "todo_create" | "todo_schedule") {
            let channel = args["schedule"]["channel"].as_str().unwrap_or("inherit");
            if channel != "inherit" {
                let quote = args["channel_evidence"].as_str().unwrap_or("");
                if !channel_authorized(channel, quote, &self.inputs) {
                    return Err(ApiError(
                        StatusCode::BAD_REQUEST,
                        "todo_channel_evidence_required",
                    ));
                }
            }
            args.as_object_mut()
                .expect("证据为对象")
                .remove("channel_evidence");
        }
        let operation_key = crate::auth::hash(&format!("{name}:{args}"));
        let source = Source {
            job: self.job,
            operation_key,
            identity_version: self.version,
        };
        args.as_object_mut().expect("证据为对象").remove("evidence");
        match name {
            "todo_create" => {
                args["idempotency_key"] = json!(self.job.id);
                let input: Create = serde_json::from_value(args)
                    .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"))?;
                service::create(self.state, &self.owner, &input, Some(&source)).await
            }
            "todo_update" => {
                let id = parse_id(&args)?;
                args.as_object_mut().expect("对象").remove("id");
                let input: Update = serde_json::from_value(args)
                    .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"))?;
                service::update(self.state, &self.owner, id, &input, Some(&source)).await
            }
            "todo_schedule" => {
                let id = parse_id(&args)?;
                let schedule_id = args.as_object_mut().expect("对象").remove("schedule_id");
                args.as_object_mut().expect("对象").remove("id");
                if let Some(id) = schedule_id {
                    args["id"] = id;
                }
                args["idempotency_key"] = json!(self.job.id);
                service::schedule(self.state, &self.owner, id, &args, Some(&source)).await
            }
            "todo_run_complete" => {
                let id = parse_id(&args)?;
                args.as_object_mut().expect("对象").remove("id");
                service::complete_run(self.state, &self.owner, id, &args, Some(&source)).await
            }
            "todo_link" => {
                let id = parse_id(&args)?;
                args.as_object_mut().expect("对象").remove("id");
                service::link(self.state, &self.owner, id, &args, Some(&source)).await
            }
            _ => Err(ApiError(StatusCode::BAD_REQUEST, "unknown_tool")),
        }
    }
}
/// UUID 仅定位资源，服务层仍重查主体和版本。
fn parse_id(args: &Value) -> ApiResult<Uuid> {
    args["id"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"))
}
impl agent_runtime::tools::Host for Provider<'_> {
    fn instructions(&self) -> String {
        format!(
            "{}\n当前发起渠道（服务端数据）：{}",
            include_str!("../../prompts/todo_tools.md"),
            self.channel
        )
    }
    fn definitions(&self) -> Vec<Value> {
        serde_json::from_str(include_str!("../../prompts/todo_tools.json"))
            .expect("固定待办 schema")
    }
    fn catalog(&self) -> Vec<agent_runtime::tools::Descriptor> {
        serde_json::from_str(include_str!("../../prompts/todo_catalog.json")).expect("固定待办目录")
    }
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            self.call(name, args)
                .await
                .unwrap_or_else(|e| json!({"error":e.1}))
        })
    }
}

/// 只接受本轮完整分句中的正向渠道依据；不允许截掉否定词后再提交引文。
fn channel_authorized(channel: &str, quote: &str, inputs: &[String]) -> bool {
    let quote = quote.trim();
    let names: &[&str] = match channel {
        "feishu" => &["飞书", "feishu"],
        "web" => &["网页", "web"],
        _ => return false,
    };
    !quote.is_empty()
        && inputs
            .iter()
            .flat_map(|input| input.split(['，', ',', '。', '；', ';', '\n']))
            .map(str::trim)
            .any(|clause| {
                let lower = clause.to_lowercase();
                clause == quote
                    && positive_target(&lower, names)
                    && !CHANNEL_NEGATIONS.iter().any(|word| lower.contains(word))
            })
}

/// 忽略渠道名两侧空格，但保留动作方向，避免把“从飞书改到网页”的来源误作目标。
fn positive_target(clause: &str, names: &[&str]) -> bool {
    let compact: String = clause.chars().filter(|c| !c.is_whitespace()).collect();
    names.iter().any(|name| {
        compact.match_indices(name).any(|(index, _)| {
            let before = &compact[..index];
            let after = &compact[index + name.len()..];
            CHANNEL_TARGET_PREFIXES
                .iter()
                .any(|prefix| before.ends_with(prefix))
                || ((before.is_empty()
                    || ["用", "使用", "通过", "在"]
                        .iter()
                        .any(|prefix| before.ends_with(prefix)))
                    && ["推送", "提醒", "通知", "发送"]
                        .iter()
                        .any(|verb| after.starts_with(verb)))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    // 验证正向分句、英文渠道和否定截取；不声称覆盖任意自然语言授权语义。
    #[test]
    fn explicit_channel_requires_positive_user_clause() {
        let inputs = vec!["我希望推送的是 feishu， 而不是网页".into()];
        assert!(channel_authorized(
            "feishu",
            "我希望推送的是 feishu",
            &inputs
        ));
        assert!(!channel_authorized("web", "而不是网页", &inputs));
        assert!(!channel_authorized("web", "网页", &inputs));
        assert!(!channel_authorized("feishu", "改到飞书", &inputs));
        let inputs = vec!["从飞书改到网页".into(), "汇总飞书资料".into()];
        assert!(channel_authorized("web", "从飞书改到网页", &inputs));
        assert!(!channel_authorized("feishu", "从飞书改到网页", &inputs));
        assert!(!channel_authorized("feishu", "汇总飞书资料", &inputs));
    }
}
