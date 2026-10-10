use super::{Connection, access::Access, connection, queries, update};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
    worker::Job,
};
use agent_runtime::tools::{Descriptor, Host};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};

// 工具目录与完整说明独立保存，只有加载后才把说明发送给模型。
const CATALOG: &str = include_str!("../../prompts/linear_catalog.json");
const DEFINITIONS: &str = include_str!("../../prompts/linear_tools.json");
const INSTRUCTIONS: &str = include_str!("../../prompts/linear_tools.md");
/// Linear 模块固定绑定本轮 owner、授权代次及真实用户输入。
pub(crate) struct Provider<'a> {
    /// API 和数据库依赖。
    state: &'a AppState,
    /// 当前生成任务及租约。
    job: &'a Job,
    /// 原始渠道身份及绑定版本，网络等待后仍须重新验证。
    access: Access,
    /// 注册时已验证的连接；执行前仍重查代次和密钥摘要。
    connection: Connection,
    /// 本批次真实用户原文，用于更新动作证据校验。
    inputs: Vec<String>,
}
impl<'a> Provider<'a> {
    /// 网页已连接且当前入口确认为本人时注册，白名单中的其他人不能发现工具。
    pub async fn new(
        state: &'a AppState,
        job: &'a Job,
        owner: &str,
        inputs: Vec<String>,
    ) -> ApiResult<Option<Self>> {
        let Some(access) = Access::new(state, owner).await? else {
            return Ok(None);
        };
        Ok(connection(state, owner).await?.map(|connection| Self {
            state,
            job,
            access,
            connection,
            inputs,
        }))
    }
}
impl Host for Provider<'_> {
    fn instructions(&self) -> String {
        format!(
            "{INSTRUCTIONS}\n当前绑定账号与工作空间（数据）：{}",
            json!({"user":self.connection.user_name,"workspace":self.connection.workspace_slug})
        )
    }
    fn catalog(&self) -> Vec<Descriptor> {
        serde_json::from_str(CATALOG).expect("固定 Linear 目录")
    }
    fn definitions(&self) -> Vec<Value> {
        serde_json::from_str(DEFINITIONS).expect("固定 Linear schema")
    }
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            let result = async {
                self.access.active(self.state, self.job).await?;
                let connection = connection::access(self.state, self.connection.generation).await?;
                let token = &super::configured(self.state)?.api_key;
                let result = if name == "linear_issue_update" {
                    update::execute(
                        self.state,
                        self.job,
                        &self.access,
                        &self.inputs,
                        &connection,
                        token,
                        args,
                    )
                    .await?
                } else {
                    queries::execute(self.state, &connection, token, name, args).await?
                };
                // 读写结果都可能包含 issue 资料，解绑、换号或取消后不再交给旧任务。
                self.access.active(self.state, self.job).await?;
                let current = super::connection(self.state, "admin").await?;
                if !current.is_some_and(|row| row.generation == connection.generation) {
                    return Err(ApiError(StatusCode::CONFLICT, "linear_connection_changed"));
                }
                Ok(result)
            }
            .await;
            result.unwrap_or_else(|error| json!({"error":error.1}))
        })
    }
}
