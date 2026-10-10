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
        include_str!("../../prompts/todo_tools.md").into()
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
