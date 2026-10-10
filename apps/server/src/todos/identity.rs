use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use sqlx::{Postgres, Transaction};

/// 将已验证渠道映射到个人主体，其他 owner 原样保留，绝不根据昵称合并。
pub async fn principal(state: &AppState, owner: &str) -> ApiResult<String> {
    Ok(sqlx::query_scalar("SELECT personal_owner($1)")
        .bind(owner)
        .fetch_one(&state.pool)
        .await?)
}
/// 本人事项只提供给网页登录本人和仍在白名单中的已绑定飞书账号。
pub async fn require_owner(state: &AppState, owner: &str) -> ApiResult<()> {
    if principal(state, owner).await? != "admin"
        || !crate::followups::policy::owner_allowed(state, owner).await?
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "personal_identity_required",
        ));
    }
    Ok(())
}
/// 保存绑定版本供长时间运行和工具宿主复核；网页不依赖渠道绑定。
pub async fn version(state: &AppState, owner: &str) -> ApiResult<Option<i64>> {
    Ok(
        sqlx::query_scalar("SELECT version FROM personal_identities WHERE owner=$1 AND enabled")
            .bind(owner)
            .fetch_optional(&state.pool)
            .await?,
    )
}
/// 在事务内锁定绑定，解绑与写入具有确定顺序，不能以旧版本完成动作。
pub async fn lock(
    tx: &mut Transaction<'_, Postgres>,
    owner: &str,
    expected: Option<i64>,
) -> ApiResult<()> {
    if owner == "admin" {
        return Ok(());
    }
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT version FROM personal_identities WHERE owner=$1 AND enabled FOR SHARE",
    )
    .bind(owner)
    .fetch_optional(&mut **tx)
    .await?;
    if current.is_none() || (expected.is_some() && current != expected) {
        return Err(ApiError(StatusCode::CONFLICT, "identity_changed"));
    }
    Ok(())
}
/// 明确解绑停止该入口尚未执行的动作，已共享事项仍属于本人。
pub async fn set_binding(
    state: &AppState,
    owner: &str,
    enabled: bool,
    expected: i64,
) -> ApiResult<()> {
    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('orbit:todos:admin'))")
        .execute(&mut *tx)
        .await?;
    if enabled {
        let verified: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_connections WHERE 'feishu:'||open_id=$1 AND status='active')")
            .bind(owner).fetch_one(&mut *tx).await?;
        if !verified {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "identity_verification_required",
            ));
        }
    }
    let changed = sqlx::query("UPDATE personal_identities SET enabled=$2,version=version+1,updated_at=now() WHERE owner=$1 AND version=$3")
        .bind(owner).bind(enabled).bind(expected).execute(&mut *tx).await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError(StatusCode::CONFLICT, "identity_changed"));
    }
    sqlx::query("SELECT id FROM conversations WHERE owner=$1 ORDER BY id FOR UPDATE")
        .bind(owner)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE memory_jobs SET status='completed' WHERE run_id IN(SELECT id FROM runs WHERE conversation_id IN(SELECT id FROM conversations WHERE owner=$1)) AND status='queued'").bind(owner).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE followup_discovery SET status='completed' WHERE owner=$1 AND status='queued'",
    )
    .bind(owner)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE runs SET status='cancelled',lease_token=NULL,partial_content='',phase='cancelled' WHERE conversation_id IN(SELECT id FROM conversations WHERE owner=$1) AND status IN('queued','running')")
        .bind(owner).execute(&mut *tx).await?;
    sqlx::query("UPDATE conversations SET context_summary='',summary_through=(SELECT COALESCE(max(seq),0) FROM runs) WHERE owner=$1")
        .bind(owner).execute(&mut *tx).await?;
    sqlx::query("UPDATE messages SET context_visible=false WHERE conversation_id IN(SELECT id FROM conversations WHERE owner=$1) AND kind='followup'")
        .bind(owner).execute(&mut *tx).await?;
    sqlx::query("UPDATE outbox SET status='cancelled',lease_token=NULL WHERE status IN('queued','running') AND (receiver_id=substring($1 from 8) OR reply_to IN(SELECT reply_to FROM runs WHERE conversation_id IN(SELECT id FROM conversations WHERE owner=$1)))")
        .bind(owner).execute(&mut *tx).await?;
    sqlx::query("UPDATE followups SET status='cancelled',error='identity_changed',version=version+1,lease_token=NULL WHERE owner=$1 AND status IN('scheduled','checking','queued')")
        .bind(owner).execute(&mut *tx).await?;
    sqlx::query("UPDATE todo_schedules SET status='paused',version=version+1,updated_at=now() WHERE delivery_owner=$1 AND status='enabled'")
        .bind(owner).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
