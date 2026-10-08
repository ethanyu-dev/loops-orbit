use crate::{AppState, communications, error::ApiResult, followups, worker::Job};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};

// 资料能力与提醒能力分别维护提示词，宿主只负责组合和路由。
const INSTRUCTIONS: &str = include_str!("../prompts/communication_tools.md");
const DEFINITIONS: &str = include_str!("../prompts/communication_tools.json");

/// 对话工具的组合宿主，身份来自服务端任务，不能由模型传入。
pub struct Host<'a> {
    /// 复用已有提醒宿主及其身份、租约和偏好。
    followups: followups::tools::Host<'a>,
    /// 注册时的资料权限快照；执行时仍重新校验。
    communications_available: bool,
}
impl<'a> Host<'a> {
    /// 只向当前授权身份提供资料工具。
    pub async fn new(state: &'a AppState, job: &'a Job) -> ApiResult<Self> {
        let followups = followups::tools::Host::new(state, job).await?;
        let communications_available =
            communications::search::allowed(state, &followups.owner).await?;
        Ok(Self {
            followups,
            communications_available,
        })
    }

    /// 保持提醒背景与现有对话流程一致。
    pub async fn background(&self) -> ApiResult<Option<String>> {
        self.followups.background().await
    }
}
impl agent_runtime::tools::Host for Host<'_> {
    /// 只有本轮具备资料权限时才提供对应的查询规则。
    fn instructions(&self) -> String {
        let mut instructions = self.followups.instructions();
        if self.communications_available {
            instructions.push_str("\n\n");
            instructions.push_str(INSTRUCTIONS);
        }
        instructions
    }

    /// 固定白名单按身份组合，运行时不接受模型自定义工具。
    fn definitions(&self) -> Vec<Value> {
        let mut definitions = self.followups.definitions();
        if self.communications_available {
            definitions.extend(
                serde_json::from_str::<Vec<Value>>(DEFINITIONS).expect("固定资料工具 schema 有效"),
            );
        }
        definitions
    }

    /// 新能力复用同一对话身份，写动作仍交给原提醒宿主。
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            if matches!(name, "communication_search" | "communication_read") {
                let host = &self.followups;
                if !self.communications_available {
                    return json!({"error":"communication_forbidden"});
                }
                // 查询也受任务租约约束，停止或替换的生成不能继续读取新资料。
                let active = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM runs r JOIN conversations c ON c.id=r.conversation_id WHERE r.id=$1 AND r.status='running' AND r.lease_token=$2 AND c.owner=$3)")
                    .bind(host.job.id).bind(host.job.lease_token).bind(&host.owner).fetch_one(&host.state.pool).await;
                match active {
                    Ok(true) => {}
                    Ok(false) => return json!({"error":"run_superseded"}),
                    Err(_) => return json!({"error":"storage_unavailable"}),
                }
                return communications::tools::execute(host.state, &host.owner, name, args)
                    .await
                    .unwrap_or_else(|error| json!({"error":error.1}));
            }
            self.followups.execute(name, args).await
        })
    }
}
