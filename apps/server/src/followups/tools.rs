use super::{Create, Source, Update, create, preferences, update};
use crate::{AppState, worker::Job};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};
use uuid::Uuid;

// 能力说明和工具 schema 独立维护，避免将用户原文拼入系统指令。
const INSTRUCTIONS: &str = include_str!("../../prompts/followup_tools.md");
const DEFINITIONS: &str = include_str!("../../prompts/followup_tools.json");
/// 单次对话的工具宿主，仅能代表该会话真实身份执行动作。
pub struct Host<'a> {
    /// 数据库与运行配置。
    pub state: &'a AppState,
    /// 每次动作都需要重新验证其租约。
    pub job: &'a Job,
    /// 从 conversations 读取的 owner。
    pub owner: String,
    /// 仅本批次用户原文可作为动作证据。
    pub inputs: Vec<String>,
    /// 本轮真实偏好快照，写入时仍需重读并校验版本。
    prefs: super::Preferences,
}
impl<'a> Host<'a> {
    /// 同批补充有顺序且不包含旧会话摘要或助手消息。
    pub async fn new(state: &'a AppState, job: &'a Job) -> crate::error::ApiResult<Self> {
        let owner: String = sqlx::query_scalar("SELECT owner FROM conversations WHERE id=$1")
            .bind(job.conversation_id)
            .fetch_one(&state.pool)
            .await?;
        let inputs=sqlx::query_scalar("SELECT input FROM runs WHERE conversation_id=$1 AND batch_id=$2 AND seq<=$3 AND status IN('running','superseded','completed') ORDER BY seq")
            .bind(job.conversation_id).bind(job.batch_id).bind(job.seq).fetch_all(&state.pool).await?;
        let prefs = preferences(state, &owner).await?;
        Ok(Self {
            state,
            job,
            owner,
            inputs,
            prefs,
        })
    }
    /// 将真实时间、时区、事项和主动消息作为带来源的数据交给模型，不伪装成用户输入。
    pub async fn background(&self) -> crate::error::ApiResult<Option<String>> {
        let tasks = self.list().await?;
        let notices:Vec<(String,chrono::DateTime<chrono::Utc>)>=sqlx::query_as("SELECT content,created_at FROM messages WHERE conversation_id=$1 AND kind='followup' AND context_visible AND created_at<=(SELECT created_at FROM runs WHERE id=$2) ORDER BY seq DESC LIMIT 5")
            .bind(self.job.conversation_id).bind(self.job.id).fetch_all(&self.state.pool).await?;
        if tasks.as_array().is_some_and(|a| a.is_empty()) && notices.is_empty() {
            return Ok(None);
        }
        Ok(Some(json!({"background_type":"followup_state","items":tasks,"sent_proactive_messages":notices}).to_string()))
    }
    /// 查询真实版本后模型才有修改目标，限制返回量避免无限增长上下文。
    async fn list(&self) -> crate::error::ApiResult<Value> {
        let rows:Vec<(Uuid,String,String,String,i64,chrono::DateTime<chrono::Utc>)>=sqlx::query_as("SELECT id,topic,kind,status,version,due_at FROM followups WHERE personal_owner(owner)=personal_owner($1) AND status IN('scheduled','checking','queued','sent') ORDER BY updated_at DESC LIMIT 100")
            .bind(&self.owner).fetch_all(&self.state.pool).await?;
        Ok(json!(rows.into_iter().map(|(id,topic,kind,status,version,due_at)|json!({"id":id,"topic":topic,"kind":kind,"status":status,"version":version,"due_at":due_at})).collect::<Vec<_>>()))
    }
    /// 拒绝助手背景或历史引用伪造为本轮动作依据；语义授权仍由固定提示词约束。
    fn evidence(&self, args: &Value) -> bool {
        args["evidence"]
            .as_str()
            .is_some_and(|e| e.chars().count() >= 2 && self.inputs.iter().any(|s| s.contains(e)))
    }
    /// 工具白名单参数在进入服务层前再次检查。
    async fn call(&self, name: &str, args: Value) -> crate::error::ApiResult<Value> {
        use crate::error::ApiError;
        use axum::http::StatusCode;
        if name == "followup_list" {
            if args.as_object().is_none_or(|a| !a.is_empty()) {
                return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"));
            }
            return self.list().await;
        }
        if name == "followup_preferences" && args.get("enabled").is_none() {
            if args.as_object().is_none_or(|fields| !fields.is_empty()) {
                return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"));
            }
            return Ok(json!(preferences(self.state, &self.owner).await?));
        }
        if !self.evidence(&args) {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "followup_evidence_required",
            ));
        }
        let source = Source {
            job: self.job,
            operation_key: crate::auth::hash(&format!("{name}:{}", args)),
            discovery_attempt: None,
        };
        match name {
            "followup_create" => {
                let input: New = serde_json::from_value(args)
                    .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"))?;
                // 聊天时间必须带偏移，不能让模型不自觉使用服务器本地时区。
                if chrono::DateTime::parse_from_rfc3339(&input.due_at).is_err() {
                    return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_followup_time"));
                }
                create(
                    self.state,
                    &self.owner,
                    &Create {
                        communication: None,
                        idempotency_key: self.job.id,
                        conversation_id: Some(self.job.conversation_id),
                        kind: "reminder".into(),
                        topic: input.topic,
                        due_at: input.due_at,
                        expires_at: None,
                        memory_ids: vec![],
                    },
                    Some(&source),
                )
                .await
            }
            "followup_update" => {
                let mut input = args;
                let id = input["id"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"))?;
                input.as_object_mut().expect("证据参数为对象").remove("id");
                input.as_object_mut().expect("对象").remove("evidence");
                let input: Update = serde_json::from_value(input)
                    .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"))?;
                update(self.state, &self.owner, id, &input, Some(&source)).await
            }
            "followup_preferences" => {
                if args.as_object().is_none_or(|fields| {
                    fields
                        .keys()
                        .any(|key| key != "enabled" && key != "evidence")
                }) {
                    return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"));
                }
                let enabled = args["enabled"]
                    .as_bool()
                    .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_arguments"))?;
                let evidence = args["evidence"].as_str().unwrap_or("");
                // 检查完整来源句，不能截掉“不要”等否定词再伪装成开启指令。
                let evidence = self
                    .inputs
                    .iter()
                    .rev()
                    .find(|input| input.contains(evidence))
                    .map(String::as_str)
                    .unwrap_or("");
                if enabled
                    && (!(evidence.contains("主动跟进") || evidence.contains("主动回访"))
                        || !["允许", "开启", "打开", "可以", "启用", "希望"]
                            .iter()
                            .any(|word| evidence.contains(word))
                        || ["不要", "不允许", "不希望", "关闭", "禁止", "别"]
                            .iter()
                            .any(|word| evidence.contains(word)))
                {
                    return Err(ApiError(
                        StatusCode::BAD_REQUEST,
                        "followup_explicit_opt_in_required",
                    ));
                }
                let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM runs WHERE id=$1 AND status='running' AND lease_token=$2)").bind(self.job.id).bind(self.job.lease_token).fetch_one(&self.state.pool).await?;
                if !active {
                    return Err(ApiError(StatusCode::CONFLICT, "run_superseded"));
                }
                let mut prefs = preferences(self.state, &self.owner).await?;
                prefs.enabled = enabled;
                super::service::save_preferences_with_source(
                    self.state,
                    &self.owner,
                    &prefs,
                    Some(&source),
                )
                .await?;
                Ok(json!(preferences(self.state, &self.owner).await?))
            }
            _ => Err(ApiError(StatusCode::BAD_REQUEST, "unknown_tool")),
        }
    }
}
/// 创建工具只接受固定字段，其余权限由宿主确定。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct New {
    /// 待提醒事项。
    topic: String,
    /// 带偏移的绝对时间。
    due_at: String,
    /// 已在宿主统一验证，不再作为生成资料。
    #[serde(rename = "evidence")]
    _evidence: String,
}
impl agent_runtime::tools::Host for Host<'_> {
    fn instructions(&self) -> String {
        format!(
            "{INSTRUCTIONS}\n当前 UTC：{}；当前身份偏好（数据）：{}",
            chrono::Utc::now().to_rfc3339(),
            serde_json::to_string(&self.prefs).expect("偏好可序列化")
        )
    }
    fn catalog(&self) -> Vec<agent_runtime::tools::Descriptor> {
        serde_json::from_str(include_str!("../../prompts/followup_catalog.json"))
            .expect("固定提醒目录")
    }
    fn definitions(&self) -> Vec<Value> {
        serde_json::from_str(DEFINITIONS).expect("固定工具 schema 有效")
    }
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            self.call(name, args)
                .await
                .unwrap_or_else(|error| json!({"error":error.1}))
        })
    }
}
