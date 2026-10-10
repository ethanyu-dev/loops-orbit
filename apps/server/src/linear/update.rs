use super::{Connection, access::Access, client, invalid, queries, unavailable};
use crate::{
    AppState, auth,
    error::{ApiError, ApiResult},
    worker::Job,
};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

// 更新只允许小范围明确字段，禁止模型发任意 mutation 或修改工作空间权限。
const MAX_DESCRIPTION_CHARS: usize = 20000;
/// 更新绑定读取版本与本轮用户证据，未提供的字段保持原样。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    /// UUID 或 issue 编号。
    issue: String,
    /// 最近一次详情返回的 updatedAt；只提供尽力冲突检测，不冒充原子 CAS。
    expected_updated_at: String,
    /// 当前输入中的逐字证据，不能从 issue 正文获得更新授权。
    evidence: String,
    /// 字段白名单保留 omitted 与 null 的区别。
    patch: Value,
}
/// 验证白名单并转为供应商字段名，空补丁和错误类型在网络前拒绝。
fn patch(input: &Value, connection: &Connection) -> ApiResult<Value> {
    let object = input
        .as_object()
        .filter(|p| !p.is_empty())
        .ok_or_else(invalid)?;
    let mut patch = json!({});
    for (key, value) in object {
        let field = match key.as_str() {
            "title"
                if value
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= 256) =>
            {
                "title"
            }
            "description"
                if value
                    .as_str()
                    .is_some_and(|s| s.chars().count() <= MAX_DESCRIPTION_CHARS) =>
            {
                "description"
            }
            "priority" if value.as_i64().is_some_and(|n| (0..=4).contains(&n)) => "priority",
            "state_id" if value.as_str().is_some_and(|s| Uuid::parse_str(s).is_ok()) => "stateId",
            "assignee_id"
                if value.is_null()
                    || value
                        .as_str()
                        .is_some_and(|s| s == "me" || Uuid::parse_str(s).is_ok()) =>
            {
                "assigneeId"
            }
            _ => return Err(invalid()),
        };
        patch[field] = if field == "assigneeId" && value == "me" {
            json!(connection.user_id)
        } else {
            value.clone()
        };
    }
    Ok(patch)
}
/// 在上游写入前持久化唯一操作记录；不确定结果不得因模型或 worker 重试再次发送。
pub(super) async fn execute(
    state: &AppState,
    job: &Job,
    access: &Access,
    inputs: &[String],
    connection: &Connection,
    token: &str,
    args: Value,
) -> ApiResult<Value> {
    let input: Update = serde_json::from_value(args).map_err(|_| invalid())?;
    if input.evidence.chars().count() < 2
        || !inputs.iter().any(|text| text.contains(&input.evidence))
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "linear_evidence_required",
        ));
    }
    let expected =
        chrono::DateTime::parse_from_rfc3339(&input.expected_updated_at).map_err(|_| invalid())?;
    let patch = patch(&input.patch, connection)?;
    let issue = queries::get(state, token, &input.issue).await?;
    let id = issue["id"]
        .as_str()
        .ok_or_else(|| unavailable("issue 标识缺失"))?;
    let key = auth::hash(&json!([job.id, connection.generation, id, patch]).to_string());
    if let Some(existing) = existing(state, &key).await? {
        return Ok(existing);
    }
    let current =
        chrono::DateTime::parse_from_rfc3339(issue["updatedAt"].as_str().ok_or_else(invalid)?)
            .map_err(unavailable)?;
    if current != expected {
        return Err(ApiError(StatusCode::CONFLICT, "linear_issue_changed"));
    }
    if let Some(id) = patch["stateId"].as_str() {
        let data = client::graphql(
            state,
            token,
            include_str!("../../graphql/linear_state.graphql"),
            json!({"id":id}),
        )
        .await?;
        if !issue["team"]["id"].is_string()
            || data["workflowState"]["team"]["id"] != issue["team"]["id"]
        {
            return Err(ApiError(StatusCode::BAD_REQUEST, "linear_state_wrong_team"));
        }
    }
    if let Some(id) = patch["assigneeId"].as_str() {
        let data = client::graphql(
            state,
            token,
            include_str!("../../graphql/linear_user.graphql"),
            json!({"id":id}),
        )
        .await?;
        if data["user"]["active"] != true
            || data["user"]["organization"]["id"] != connection.workspace_id
        {
            return Err(ApiError(StatusCode::BAD_REQUEST, "linear_assignee_invalid"));
        }
    }
    let mut tx = state.pool.begin().await?;
    // 绑定锁持有到派发记录提交，解绑先完成时不能继续登记更新。
    access.lock(&mut tx).await?;
    // 与断开和重连共用连接锁；落盘后请求才可能发送，之后取消不能撤回平台已接受的修改。
    let allowed: Option<Uuid> = sqlx::query_scalar(
        "SELECT generation FROM linear_connections WHERE owner='admin' AND generation=$1 AND status='active' AND key_fingerprint=$2 FOR UPDATE",
    )
    .bind(connection.generation)
    .bind(&connection.key_fingerprint)
    .fetch_optional(&mut *tx)
    .await?;
    if allowed.is_none() {
        return Err(ApiError(StatusCode::CONFLICT, "linear_connection_changed"));
    }
    access.active(state, job).await?;
    let inserted = sqlx::query(include_str!("../sql/linear_operation_claim.sql"))
        .bind(&key)
        .bind(job.id)
        .bind(connection.generation)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    if inserted == 0 {
        return Ok(existing(state, &key)
            .await?
            .unwrap_or(json!({"status":"unknown"})));
    }
    let response = client::graphql(
        state,
        token,
        include_str!("../../graphql/linear_update.graphql"),
        json!({"id":id,"input":patch}),
    )
    .await;
    let (status, result) = match response {
        Ok(data)
            if data["issueUpdate"]["success"] == true
                && data["issueUpdate"]["issue"]["id"] == id =>
        {
            (
                "confirmed",
                json!({"status":"confirmed","issue":data["issueUpdate"]["issue"],"changed_fields":input.patch.as_object().expect("已验证补丁").keys().collect::<Vec<_>>()}),
            )
        }
        Err(error)
            if matches!(
                error.1,
                "linear_invalid_key" | "linear_permission_denied" | "linear_rate_limited"
            ) =>
        {
            (
                "rejected",
                json!({"status":"rejected","issue_id":id,"error":error.1,"retry_safe":false}),
            )
        }
        _ => (
            "unknown",
            json!({"status":"unknown","issue_id":id,"error":"linear_update_outcome_unknown","retry_safe":false}),
        ),
    };
    sqlx::query("UPDATE linear_operations SET status=$2,result=$3 WHERE operation_key=$1")
        .bind(&key)
        .bind(status)
        .bind(&result)
        .execute(&state.pool)
        .await?;
    Ok(result)
}
/// 正在发送或进程中断后遗留的操作同样视为未知，不能自动重新执行。
async fn existing(state: &AppState, key: &str) -> ApiResult<Option<Value>> {
    let row: Option<(String, Option<Value>)> =
        sqlx::query_as("SELECT status,result FROM linear_operations WHERE operation_key=$1")
            .bind(key)
            .fetch_optional(&state.pool)
            .await?;
    Ok(row.map(|(status,result)|result.unwrap_or(json!({"status":"unknown","dispatch_status":status,"error":"linear_update_outcome_unknown","retry_safe":false}))))
}
