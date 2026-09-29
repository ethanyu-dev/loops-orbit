use super::Preferences;
use crate::{
    AppState,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use chrono::{DateTime, NaiveDateTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use uuid::Uuid;

// 限定调度跨度与文本大小；时间解析失败必须澄清，不能猜夏令时重叠时刻。
pub const MAX_TOPIC_CHARS: usize = 500;
pub const MAX_HORIZON_DAYS: i64 = 366;

/// 统一校验偏好，避免无效时区导致调度器永久重试。
pub fn validate(prefs: &Preferences) -> ApiResult<()> {
    if prefs.timezone.parse::<Tz>().is_err()
        || !(0..1440).contains(&prefs.quiet_start)
        || !(0..1440).contains(&prefs.quiet_end)
        || !(60..=10080).contains(&prefs.min_interval_minutes)
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_followup_preferences",
        ));
    }
    Ok(())
}
/// 支持显式偏移和网页本地时间；有歧义或不存在的 DST 时间拒绝保存。
pub fn parse_time(value: &str, timezone: &str) -> ApiResult<DateTime<Utc>> {
    if let Ok(time) = DateTime::parse_from_rfc3339(value) {
        return Ok(time.with_timezone(&Utc));
    }
    let zone: Tz = timezone
        .parse()
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_timezone"))?;
    let local = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M")
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_followup_time"))?;
    zone.from_local_datetime(&local)
        .single()
        .map(|v| v.with_timezone(&Utc))
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "ambiguous_followup_time"))
}
/// 按实际时区判断静默，跨午夜与夏令时均不使用固定 UTC 偏移。
pub fn is_quiet(prefs: &Preferences, now: DateTime<Utc>) -> bool {
    let zone: Tz = prefs.timezone.parse().expect("已校验持久化偏好");
    let local = now.with_timezone(&zone);
    let minute = (local.hour() * 60 + local.minute()) as i32;
    if prefs.quiet_start < prefs.quiet_end {
        minute >= prefs.quiet_start && minute < prefs.quiet_end
    } else if prefs.quiet_start > prefs.quiet_end {
        minute >= prefs.quiet_start || minute < prefs.quiet_end
    } else {
        false
    }
}
/// 使用绝对时间推进，跨 DST 时不会创建不存在的本地时间。
pub fn next_awake(prefs: &Preferences, mut now: DateTime<Utc>) -> DateTime<Utc> {
    for _ in 0..(26 * 60) {
        if !is_quiet(prefs, now) {
            return now;
        }
        now += chrono::Duration::minutes(1);
    }
    now
}
/// 出站前仍检查访客授权与飞书白名单，撤销后不会继续主动联系。
pub async fn owner_allowed(state: &AppState, owner: &str) -> ApiResult<bool> {
    if owner == "admin" {
        return Ok(true);
    }
    if let Some(id) = owner
        .strip_prefix("guest:")
        .and_then(|s| Uuid::parse_str(s).ok())
    {
        return Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM grants WHERE id=$1 AND revoked_at IS NULL AND expires_at>now())").bind(id).fetch_one(&state.pool).await?);
    }
    Ok(owner.strip_prefix("feishu:").is_some_and(|id| {
        state
            .config
            .feishu
            .as_ref()
            .is_some_and(|c| c.allowed_users.iter().any(|allowed| allowed == id))
    }))
}

/// 直接编辑、删除或到期的本地原文不再支持旧事项；读取失败时保留任务等待恢复。
pub async fn dependencies_valid(state: &AppState, job: &super::Followup) -> ApiResult<bool> {
    if !crate::communications::dependencies::valid(
        state,
        &job.owner,
        &job.memory_versions["_communication"],
    )
    .await?
    {
        return Ok(false);
    }
    if job.memory_ids.is_empty() {
        return Ok(true);
    }
    let Some(config) = &state.config.memory else {
        return Ok(false);
    };
    let current = crate::memory::store::read(&config.directory, &job.owner)
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "memory_unavailable"))?;
    Ok(job.memory_ids.iter().all(|id| {
        current.iter().any(|entry| {
            entry.id == *id
                && entry.active()
                && job.memory_versions[id.to_string()].as_str() == Some(entry.hash().as_str())
        })
    }))
}
