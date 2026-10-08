use super::{Connection, client, connection, queries, update};
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
    /// 注册时已验证的连接；执行前仍重查代次和 scopes。
    connection: Connection,
    /// 本批次真实用户原文，用于更新动作证据校验。
    inputs: Vec<String>,
}
impl<'a> Provider<'a> {
    /// 只有管理员连接成功后注册；未连接和其他身份不能发现这些工具。
    pub async fn new(
        state: &'a AppState,
        job: &'a Job,
        owner: &str,
        inputs: Vec<String>,
    ) -> ApiResult<Option<Self>> {
        Ok(connection(state, owner).await?.map(|connection| Self {
            state,
            job,
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
        serde_json::from_str::<Vec<Descriptor>>(CATALOG)
            .expect("固定 Linear 目录")
            .into_iter()
            .filter(|tool| {
                tool.effect == "read" || self.connection.scopes.iter().any(|scope| scope == "write")
            })
            .collect()
    }
    fn definitions(&self) -> Vec<Value> {
        serde_json::from_str::<Vec<Value>>(DEFINITIONS)
            .expect("固定 Linear schema")
            .into_iter()
            .filter(|tool| {
                self.catalog()
                    .iter()
                    .any(|entry| tool["function"]["name"] == entry.name)
            })
            .collect()
    }
    fn execute<'a>(
        &'a self,
        name: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Value> + Send + 'a>> {
        Box::pin(async move {
            let result = async {
                update::active(self.state, self.job).await?;
                let (connection, tokens) =
                    client::access(self.state, self.connection.generation).await?;
                if !connection.scopes.iter().any(|s| s == "read") {
                    return Err(super::invalid());
                }
                if name == "linear_issue_update" {
                    update::execute(
                        self.state,
                        self.job,
                        &self.inputs,
                        &connection,
                        &tokens.access_token,
                        args,
                    )
                    .await
                } else {
                    let result =
                        queries::execute(self.state, &connection, &tokens.access_token, name, args)
                            .await?;
                    // 网络等待期间可能断开、换号或取消，返回资料前再次校验，避免旧任务继续接收数据。
                    update::active(self.state, self.job).await?;
                    let current = super::connection(self.state, "admin").await?;
                    if !current.is_some_and(|row| row.generation == connection.generation) {
                        return Err(ApiError(StatusCode::CONFLICT, "linear_connection_changed"));
                    }
                    Ok(result)
                }
            }
            .await;
            result.unwrap_or_else(|error| json!({"error":error.1}))
        })
    }
}
