use super::{Connection, client, invalid, unavailable};
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

// 列表只取摘要字段；长描述独立分页，避免工具结果挤满模型上下文。
const MAX_PAGE: usize = 50;
const DEFAULT_PAGE: usize = 20;
const DESCRIPTION_CHARS: usize = 6000;
/// issue 列表查询默认只返回绑定账号负责的事项。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    /// me、all 或真实用户 UUID。
    assignee: Option<String>,
    /// 可选团队 UUID。
    team_id: Option<Uuid>,
    /// 可选状态 UUID。
    state_id: Option<Uuid>,
    /// 标题字面匹配，非全站自然语言搜索。
    query: Option<String>,
    /// 请求页大小。
    limit: Option<usize>,
    /// 上游游标，原样转交固定查询。
    cursor: Option<String>,
}
/// 详情支持描述续读与版本校验。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Get {
    /// UUID 或例如 ENG-123 的编号。
    issue: String,
    /// 描述的 Unicode 字符偏移。
    #[serde(default)]
    description_offset: usize,
    /// 续读时必须携带之前返回的更新时间。
    expected_updated_at: Option<String>,
}
/// 团队元数据按类型分别分页，避免某一子列表无声截断。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    /// teams、states 或 members。
    kind: String,
    /// states 和 members 必须指定团队。
    team_id: Option<Uuid>,
    /// 请求页大小。
    limit: Option<usize>,
    /// 上游游标。
    cursor: Option<String>,
}
/// 只接受有界的 ID 或 issue 编号，不让用户输入改变 GraphQL 结构。
pub(super) fn issue_id(value: &str) -> ApiResult<()> {
    if value.is_empty()
        || value.len() > 100
        || !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(invalid());
    }
    Ok(())
}
/// 页大小与游标长度在请求前校验。
fn page(limit: Option<usize>, cursor: &Option<String>) -> ApiResult<usize> {
    let limit = limit.unwrap_or(DEFAULT_PAGE);
    if limit == 0 || limit > MAX_PAGE || cursor.as_ref().is_some_and(|c| c.len() > 2048) {
        return Err(invalid());
    }
    Ok(limit)
}
/// 将供应商分页转换为明确的覆盖信息，不伪造全量数量。
fn collection(value: &Value) -> ApiResult<Value> {
    let nodes = value["nodes"]
        .as_array()
        .ok_or_else(|| unavailable("列表缺失"))?;
    let more = value["pageInfo"]["hasNextPage"]
        .as_bool()
        .ok_or_else(|| unavailable("分页信息缺失"))?;
    if more && !value["pageInfo"]["endCursor"].is_string() {
        return Err(unavailable("缺少后续游标"));
    }
    Ok(
        json!({"items":nodes,"returned_count":nodes.len(),"has_more":more,"next_cursor":if more {value["pageInfo"]["endCursor"].clone()} else {Value::Null}}),
    )
}
/// 工具名称映射到固定文档，参数仅作为 GraphQL variables 发送。
pub(super) async fn execute(
    state: &AppState,
    connection: &Connection,
    token: &str,
    name: &str,
    args: Value,
) -> ApiResult<Value> {
    let mut result = match name {
        "linear_issue_list" => {
            let input: List = serde_json::from_value(args).map_err(|_| invalid())?;
            let limit = page(input.limit, &input.cursor)?;
            let mut filter = json!({});
            let assignee = input.assignee.as_deref().unwrap_or("me");
            match assignee {
                "me" => filter["assignee"] = json!({"id":{"eq":connection.user_id}}),
                "all" => {}
                value => {
                    Uuid::parse_str(value).map_err(|_| invalid())?;
                    filter["assignee"] = json!({"id":{"eq":value}});
                }
            }
            if let Some(id) = input.team_id {
                filter["team"] = json!({"id":{"eq":id}});
            }
            if let Some(id) = input.state_id {
                filter["state"] = json!({"id":{"eq":id}});
            }
            if let Some(query) = input.query {
                if query.chars().count() > 200 {
                    return Err(invalid());
                }
                filter["title"] = json!({"containsIgnoreCase":query});
            }
            let data = client::graphql(
                state,
                token,
                include_str!("../../graphql/linear_issues.graphql"),
                json!({"filter":filter,"first":limit,"after":input.cursor}),
            )
            .await?;
            let mut result = collection(&data["issues"])?;
            result["filter"] = filter;
            result
        }
        "linear_issue_get" => {
            let input: Get = serde_json::from_value(args).map_err(|_| invalid())?;
            let mut issue = get(state, token, &input.issue).await?;
            if (input.description_offset > 0 && input.expected_updated_at.is_none())
                || input
                    .expected_updated_at
                    .as_ref()
                    .is_some_and(|expected| issue["updatedAt"].as_str() != Some(expected))
            {
                return Err(ApiError(StatusCode::CONFLICT, "linear_issue_changed"));
            }
            let chars: Vec<char> = issue["description"]
                .as_str()
                .unwrap_or("")
                .chars()
                .collect();
            if input.description_offset > chars.len() {
                return Err(invalid());
            }
            let end = (input.description_offset + DESCRIPTION_CHARS).min(chars.len());
            issue["description"] = json!(
                chars[input.description_offset..end]
                    .iter()
                    .collect::<String>()
            );
            json!({"issue":issue,"description_offset":input.description_offset,"description_total_chars":chars.len(),"next_description_offset":(end<chars.len()).then_some(end),"has_more":end<chars.len()})
        }
        "linear_team_metadata" => {
            let input: Metadata = serde_json::from_value(args).map_err(|_| invalid())?;
            let limit = page(input.limit, &input.cursor)?;
            let (query, path) = match input.kind.as_str() {
                "teams" => (include_str!("../../graphql/linear_teams.graphql"), "/teams"),
                "states" if input.team_id.is_some() => (
                    include_str!("../../graphql/linear_states.graphql"),
                    "/team/states",
                ),
                "members" if input.team_id.is_some() => (
                    include_str!("../../graphql/linear_members.graphql"),
                    "/team/members",
                ),
                _ => return Err(invalid()),
            };
            let data = client::graphql(
                state,
                token,
                query,
                json!({"id":input.team_id,"first":limit,"after":input.cursor}),
            )
            .await?;
            collection(
                data.pointer(path)
                    .ok_or_else(|| unavailable("缺少团队数据"))?,
            )?
        }
        _ => return Err(invalid()),
    };
    result["workspace"] = json!({"id":connection.workspace_id,"name":connection.workspace_name,"slug":connection.workspace_slug});
    Ok(result)
}
/// 更新前和详情读取共用新鲜 issue，调用者负责限制最终返回的描述长度。
pub(super) async fn get(state: &AppState, token: &str, id: &str) -> ApiResult<Value> {
    issue_id(id)?;
    let data = client::graphql(
        state,
        token,
        include_str!("../../graphql/linear_issue.graphql"),
        json!({"id":id}),
    )
    .await?;
    let issue = data["issue"].clone();
    if !issue["id"].is_string() || !issue["updatedAt"].is_string() {
        return Err(ApiError(StatusCode::NOT_FOUND, "linear_issue_not_found"));
    }
    Ok(issue)
}
