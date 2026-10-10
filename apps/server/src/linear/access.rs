use crate::{
    AppState, auth,
    error::{ApiError, ApiResult},
    todos::identity,
    worker::Job,
};
use axum::http::StatusCode;
use sqlx::{Postgres, Transaction};

/// 本轮工具固定真实渠道身份及绑定版本，模型不能指定或替换授权主体。
pub(super) struct Access {
    /// 来自会话记录的原始身份，不能提前归一化为 admin。
    owner: String,
    /// 飞书绑定代次，防止解绑后重新绑定使旧宿主恢复权限。
    version: Option<i64>,
}
impl Access {
    /// 先记录版本再检查本人身份；并发变更由执行时的版本复核拒绝。
    pub(super) async fn new(state: &AppState, owner: &str) -> ApiResult<Option<Self>> {
        let version = identity::version(state, owner).await?;
        if !auth::is_account_owner(state, owner).await? || (owner != "admin" && version.is_none()) {
            return Ok(None);
        }
        Ok(Some(Self {
            owner: owner.into(),
            version,
        }))
    }

    /// 请求前及返回资料前同时复核本人、白名单、绑定版本、会话身份和任务租约。
    pub(super) async fn active(&self, state: &AppState, job: &Job) -> ApiResult<()> {
        let allowed = auth::is_account_owner(state, &self.owner).await?;
        let active: bool = sqlx::query_scalar(include_str!("../sql/linear_run_active.sql"))
            .bind(job.id)
            .bind(job.lease_token)
            .bind(&self.owner)
            .bind(self.version)
            .fetch_one(&state.pool)
            .await?;
        if allowed && active {
            Ok(())
        } else {
            Err(ApiError(StatusCode::CONFLICT, "run_superseded"))
        }
    }

    /// 与解绑共用绑定行锁，只有当前代次能够登记外部更新的派发记录。
    pub(super) async fn lock(&self, tx: &mut Transaction<'_, Postgres>) -> ApiResult<()> {
        identity::lock(tx, &self.owner, self.version).await
    }
}
