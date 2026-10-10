use crate::error::{ApiError, ApiResult};
use axum::http::StatusCode;
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;

// 最长向前寻找十年；按日期计算避免跨夏令时后当地钟点漂移。
const MAX_DAYS: i64 = 3660;
/// 返回基准时刻之后的下一期；月末按目标月最后一天，DST 缺失/重复钟点跳过。
pub fn next(
    anchor: DateTime<Utc>,
    after: DateTime<Utc>,
    zone: &str,
    rule: &str,
) -> ApiResult<Option<DateTime<Utc>>> {
    if rule == "once" {
        return Ok(None);
    }
    let zone: Tz = zone
        .parse()
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_timezone"))?;
    let local = anchor.with_timezone(&zone);
    let start = after
        .with_timezone(&zone)
        .date_naive()
        .max(local.date_naive());
    for day in 0..MAX_DAYS {
        let date = start + Duration::days(day);
        let matches = match rule {
            "daily" => true,
            "weekdays" => date.weekday().num_days_from_monday() < 5,
            "weekly" => date.weekday() == local.weekday(),
            "monthly" => {
                let (year, month) = if date.month() == 12 {
                    (date.year() + 1, 1)
                } else {
                    (date.year(), date.month() + 1)
                };
                let last = (NaiveDate::from_ymd_opt(year, month, 1).expect("有效月份")
                    - Duration::days(1))
                .day();
                date.day() == local.day().min(last)
            }
            _ => return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_recurrence")),
        };
        if matches
            && let Some(candidate) = zone
                .from_local_datetime(&date.and_time(local.time()))
                .single()
        {
            let candidate = candidate.with_timezone(&Utc);
            if candidate > after && candidate >= anchor {
                return Ok(Some(candidate));
            }
        }
    }
    Err(ApiError(StatusCode::BAD_REQUEST, "invalid_recurrence"))
}

#[cfg(test)]
mod tests {
    use super::*;
    // 验证周末跳过和当地钟点跨 DST 保持；不代表实际服务器准点投递。
    #[test]
    fn weekdays_and_dst() {
        let a = DateTime::parse_from_rfc3339("2026-03-06T09:00:00-05:00")
            .unwrap()
            .with_timezone(&Utc);
        let n = next(a, a, "America/New_York", "weekdays").unwrap().unwrap();
        assert_eq!(n.to_rfc3339(), "2026-03-09T13:00:00+00:00");
    }
    // 验证月末收缩后仍以原始 31 日为锚；不验证任意 cron 表达式。
    #[test]
    fn month_end_keeps_anchor() {
        let a = DateTime::parse_from_rfc3339("2026-01-31T09:00:00+08:00")
            .unwrap()
            .with_timezone(&Utc);
        let b = next(a, a, "Asia/Shanghai", "monthly").unwrap().unwrap();
        let c = next(a, b, "Asia/Shanghai", "monthly").unwrap().unwrap();
        assert_eq!(b.with_timezone(&chrono_tz::Asia::Shanghai).day(), 28);
        assert_eq!(c.with_timezone(&chrono_tz::Asia::Shanghai).day(), 31);
    }
}
