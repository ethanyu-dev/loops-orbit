use crate::{
    AppState,
    error::{ApiError, ApiResult},
    followups::Followup,
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};

/// 定时执行宿主仅提供读取，任何调用都重新核对本次处理租约与安排状态。
struct ReadHost<'a> {
    /// 当前应用的资料与连接权限。
    state: &'a AppState,
    /// 本次生成的真实租约和投递版本。
    job: &'a Followup,
}
impl agent_runtime::tools::Host for ReadHost<'_> {
    fn definitions(&self) -> Vec<Value> {
        let mut all: Vec<Value> =
            serde_json::from_str(include_str!("../../prompts/communication_tools.json"))
                .expect("固定 schema");
        let linear: Vec<Value> =
            serde_json::from_str(include_str!("../../prompts/linear_tools.json"))
                .expect("固定 schema");
        all.extend(
            linear
                .into_iter()
                .filter(|d| d["function"]["name"] != "linear_issue_update"),
        );
        all
    }
    fn catalog(&self) -> Vec<agent_runtime::tools::Descriptor> {
        let mut all: Vec<agent_runtime::tools::Descriptor> =
            serde_json::from_str(include_str!("../../prompts/communication_catalog.json"))
                .expect("固定目录");
        let linear: Vec<agent_runtime::tools::Descriptor> =
            serde_json::from_str(include_str!("../../prompts/linear_catalog.json"))
                .expect("固定目录");
        all.extend(
            linear
                .into_iter()
                .filter(|d| d.name != "linear_issue_update"),
        );
        all
    }
    fn instructions(&self) -> String {
        format!(
            "{}\n{}",
            include_str!("../../prompts/communication_tools.md"),
            include_str!("../../prompts/todo_execute.md")
        )
    }
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            let result=async {
                let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM followups WHERE id=$1 AND version=$2 AND lease_token=$3 AND status='checking')").bind(self.job.id).bind(self.job.version).bind(self.job.lease_token).fetch_one(&self.state.pool).await?;
                if !active || !super::scheduler::delivery_allowed(self.state,self.job.id).await? {return Err(ApiError(StatusCode::CONFLICT,"todo_schedule_changed"));}
                match name {
                    "communication_search"|"communication_read" => crate::communications::tools::execute(self.state,"admin",name,args).await,
                    "linear_issue_list"|"linear_issue_get"|"linear_team_metadata"=>crate::linear::read_for_todo(self.state,name,args).await,
                    _=>Err(ApiError(StatusCode::FORBIDDEN,"todo_read_only")),
                }
            }.await;
            result.unwrap_or_else(|e| json!({"error":e.1}))
        })
    }
}
/// 有 execute 安排时生成本次报告；外层限时小于跟进租约，失败不会输出假成功。
pub async fn generate(state: &AppState, job: &Followup) -> ApiResult<Option<String>> {
    let data:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('todo',to_jsonb(t),'instruction',s.instruction,'timezone',s.timezone) FROM todo_schedules s JOIN todos t ON t.id=s.todo_id JOIN followups f ON f.todo_schedule_id=s.id WHERE f.id=$1 AND s.kind='execute'")
        .bind(job.id).fetch_optional(&state.pool).await?;
    let Some(mut data) = data else {
        return Ok(None);
    };
    let revision = revision(state).await?;
    let linked: Vec<Value> = sqlx::query_scalar("SELECT jsonb_build_object('kind',l.kind,'resource_id',l.resource_id,'label',l.label) FROM todo_links l JOIN todo_schedules s ON s.todo_id=l.todo_id JOIN followups f ON f.todo_schedule_id=s.id WHERE f.id=$1 ORDER BY l.id LIMIT 50")
        .bind(job.id).fetch_all(&state.pool).await?;
    data["linked_resources"] = json!(linked);
    let query = format!(
        "{} {}",
        job.topic,
        data["instruction"].as_str().unwrap_or("")
    );
    let hits = crate::rag::retrieve(state, &query, Some("admin")).await?;
    let knowledge = crate::knowledge::published(&hits);
    data["published_knowledge"] = json!(crate::knowledge::context(&knowledge));
    data["related_communications"] = json!(crate::rag::private_context(&hits));
    let saved=sqlx::query("UPDATE followups SET memory_versions=jsonb_set(memory_versions,'{_todo_execution_revision}',$4) WHERE id=$1 AND version=$2 AND lease_token=$3 AND status='checking'")
        .bind(job.id).bind(job.version).bind(job.lease_token).bind(&revision).execute(&state.pool).await?;
    if saved.rows_affected() == 0 {
        return Err(ApiError(StatusCode::CONFLICT, "todo_schedule_changed"));
    }
    let host = ReadHost { state, job };
    let history = vec![
        agent_runtime::Message {
            role: "system".into(),
            content: include_str!("../../prompts/todo_execute.md").into(),
        },
        agent_runtime::Message {
            role: "user".into(),
            content: data.to_string(),
        },
    ];
    let answer = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        state.runtime.run_with_tools(&history, None, Some(&host)),
    )
    .await
    .map_err(|_| ApiError(StatusCode::GATEWAY_TIMEOUT, "todo_execution_timeout"))?
    .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "todo_execution_failed"))?;
    if revision != self::revision(state).await?
        || !super::scheduler::delivery_allowed(state, job.id).await?
    {
        return Err(ApiError(StatusCode::CONFLICT, "todo_sources_changed"));
    }
    Ok(Some(answer))
}

/// 保存无正文的来源代次快照；任何资料订阅、版本或连接变化都使在途报告失效。
async fn revision(state: &AppState) -> ApiResult<Value> {
    let snapshot:Value=sqlx::query_scalar("SELECT jsonb_build_object('knowledge',(SELECT revision FROM knowledge_state),'communications',COALESCE((SELECT md5(string_agg(id::text||':'||version::text||':'||enabled::text||':'||removal_pending::text,',' ORDER BY id)) FROM communication_sources),''),'documents',COALESCE((SELECT md5(string_agg(id::text||':'||version::text||':'||raw_hash||':'||COALESCE(summary_hash,''),',' ORDER BY id)) FROM communication_documents),''),'connection',(SELECT version FROM communication_connections WHERE owner='admin'))")
        .fetch_one(&state.pool).await?;
    Ok(json!({"sources":snapshot,"linear":crate::linear::todo_revision(state).await?}))
}
/// 投递队列可能等待很久，必须重新核对生成时的来源快照。
pub async fn dependencies_valid(state: &AppState, id: uuid::Uuid) -> ApiResult<bool> {
    let old: Option<Value> = sqlx::query_scalar(
        "SELECT memory_versions->'_todo_execution_revision' FROM followups WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;
    match old {
        Some(old) => Ok(old == revision(state).await?),
        None => Ok(true),
    }
}
