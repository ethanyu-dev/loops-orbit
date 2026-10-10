use crate::{AppState, communications, error::ApiResult, followups, worker::Job};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};

// 资料能力与提醒能力分别维护提示词，宿主只负责组合和路由。
const INSTRUCTIONS: &str = include_str!("../prompts/communication_tools.md");
const DEFINITIONS: &str = include_str!("../prompts/communication_tools.json");

/// 对话工具宿主通过注册表组合各模块，保留提醒背景的已有入口。
pub struct Host<'a> {
    /// 已有提醒宿主同时提供独立背景状态。
    followups: std::sync::Arc<followups::tools::Host<'a>>,
    /// 只注册当前身份可用的业务模块。
    registry: agent_runtime::tools::Registry<'a>,
    /// 本人待办背景与业务工具使用同一身份。
    todos: Option<std::sync::Arc<crate::todos::tools::Provider<'a>>>,
}
impl<'a> Host<'a> {
    /// 业务模块在此注册，运行时不再维护名称分支。
    pub async fn new(state: &'a AppState, job: &'a Job) -> ApiResult<Self> {
        let followups = std::sync::Arc::new(followups::tools::Host::new(state, job).await?);
        let mut providers: Vec<std::sync::Arc<dyn agent_runtime::tools::Host + 'a>> =
            vec![followups.clone()];
        if crate::auth::is_account_owner(state, &followups.owner).await?
            && communications::search::allowed(state, &followups.owner).await?
        {
            providers.push(std::sync::Arc::new(Communications {
                state,
                job,
                owner: followups.owner.clone(),
            }));
        }
        if let Some(linear) = crate::linear::tools::Provider::new(
            state,
            job,
            &followups.owner,
            followups.inputs.clone(),
        )
        .await?
        {
            providers.push(std::sync::Arc::new(linear));
        }
        let todos = crate::todos::tools::Provider::new(
            state,
            job,
            &followups.owner,
            followups.inputs.clone(),
        )
        .await?
        .map(std::sync::Arc::new);
        if let Some(todos) = &todos {
            providers.push(todos.clone());
        }
        Ok(Self {
            todos,
            followups,
            registry: agent_runtime::tools::Registry::new(providers),
        })
    }
    /// 提醒背景独立于工具加载，不主动泄露其他模块资料。
    pub async fn background(&self) -> ApiResult<Option<String>> {
        let mut parts = self
            .followups
            .background()
            .await?
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(todos) = &self.todos {
            parts.extend(todos.background().await?);
        }
        Ok((!parts.is_empty()).then(|| parts.join("\n")))
    }
}
impl agent_runtime::tools::Host for Host<'_> {
    fn instructions(&self) -> String {
        String::new()
    }
    fn definitions(&self) -> Vec<Value> {
        self.registry.definitions()
    }
    fn catalog(&self) -> Vec<agent_runtime::tools::Descriptor> {
        self.registry.catalog()
    }
    fn definition(&self, name: &str) -> Option<Value> {
        self.registry.definition(name)
    }
    fn instructions_for(&self, names: &[String]) -> String {
        self.registry.instructions_for(names)
    }
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        self.registry.execute(name, args)
    }
}

/// 沟通模块的只读适配器，执行时重新验证任务和资料权限。
struct Communications<'a> {
    /// 数据库与配置。
    state: &'a AppState,
    /// 当前任务的租约。
    job: &'a Job,
    /// 由对话解析的真实身份。
    owner: String,
}
impl agent_runtime::tools::Host for Communications<'_> {
    fn instructions(&self) -> String {
        INSTRUCTIONS.into()
    }
    fn definitions(&self) -> Vec<Value> {
        serde_json::from_str(DEFINITIONS).expect("固定资料 schema")
    }
    fn catalog(&self) -> Vec<agent_runtime::tools::Descriptor> {
        serde_json::from_str(include_str!("../prompts/communication_catalog.json"))
            .expect("固定资料目录")
    }
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            let active=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM runs r JOIN conversations c ON c.id=r.conversation_id WHERE r.id=$1 AND r.status='running' AND r.lease_token=$2 AND c.owner=$3)")
                .bind(self.job.id).bind(self.job.lease_token).bind(&self.owner).fetch_one(&self.state.pool).await;
            match active {
                Ok(true) => {}
                Ok(false) => return json!({"error":"run_superseded"}),
                Err(_) => return json!({"error":"storage_unavailable"}),
            }
            communications::tools::execute(self.state, &self.owner, name, args)
                .await
                .unwrap_or_else(|error| json!({"error":error.1}))
        })
    }
}
