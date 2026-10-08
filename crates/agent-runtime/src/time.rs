use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

/// 时间查询只接受 IANA 时区，不接受客户端伪造当前时间。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Arguments {
    /// 省略时兼容原有 UTC 工具；自然日查询应显式传入资料使用的时区。
    #[serde(default)]
    timezone: Option<String>,
}

/// 单次采样产生 UTC 和当地时间，避免跨午夜时日期与时间不一致。
pub(crate) fn current(args: Arguments, now: DateTime<Utc>) -> Value {
    let Ok(timezone) = args
        .timezone
        .as_deref()
        .unwrap_or("UTC")
        .parse::<chrono_tz::Tz>()
    else {
        return json!({"error":"invalid_timezone"});
    };
    let local = now.with_timezone(&timezone);
    json!({"utc":now.to_rfc3339(),"timezone":timezone.name(),"local_time":local.to_rfc3339(),"local_date":local.format("%Y-%m-%d").to_string()})
}

#[cfg(test)]
mod tests {
    use super::*;

    // 固定时刻验证北京时间跨日、夏令时偏移与非法参数；不验证模型自行选择时区的能力。
    #[test]
    fn local_dates_and_timezone_validation() {
        let now = DateTime::parse_from_rfc3339("2026-10-07T16:05:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let query = |zone: &str| {
            current(
                Arguments {
                    timezone: Some(zone.into()),
                },
                now,
            )
        };
        assert_eq!(query("Asia/Shanghai")["local_date"], "2026-10-08");
        assert_eq!(
            query("Asia/Shanghai")["local_time"],
            "2026-10-08T00:05:00+08:00"
        );
        assert_eq!(
            query("America/New_York")["local_time"],
            "2026-10-07T12:05:00-04:00"
        );
        assert_eq!(query("invalid")["error"], "invalid_timezone");
        assert!(serde_json::from_value::<Arguments>(json!({"timezone":42})).is_err());
        assert!(serde_json::from_value::<Arguments>(json!({"now":"tomorrow"})).is_err());
    }
}
