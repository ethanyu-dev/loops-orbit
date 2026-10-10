use crate::error::ApiResult;
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

// 工具循环有界；额外一条用于识别是否需要提示还有其他已提交操作。
const MAX_RECEIPTS: i64 = 100;

/// 只返回确认保存所需的安排字段，避免把身份和接收人暴露给模型。
pub(super) async fn schedule_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> ApiResult<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('id',s.id,'title',t.title,'channel',c.channel,'status',s.status,'next_run_at',s.next_run_at,'timezone',s.timezone,'recurrence',s.recurrence,'version',s.version) FROM todo_schedules s JOIN conversations c ON c.id=s.conversation_id JOIN todos t ON t.id=s.todo_id WHERE s.id=$1")
        .bind(id).fetch_one(&mut **tx).await?)
}

/// 同事务写入的结果是唯一成功依据；不从模型半截回复推断已经执行。
pub(crate) async fn saved_reply(
    tx: &mut Transaction<'_, Postgres>,
    run: Uuid,
) -> ApiResult<Option<String>> {
    let results: Vec<Value> = sqlx::query_scalar(
        "SELECT result FROM todo_operations WHERE run_id=$1 ORDER BY operation_key LIMIT $2",
    )
    .bind(run)
    .bind(MAX_RECEIPTS + 1)
    .fetch_all(&mut **tx)
    .await?;
    if results.is_empty() {
        return Ok(None);
    }
    let mut reply = String::from(
        "本次已保存待办操作，但后续回复生成失败。已提交的操作不会因此撤销，也无需重复提交。\n",
    );
    for result in results.iter().take(MAX_RECEIPTS as usize) {
        if let Some(schedule) = result.get("schedule").filter(|s| s.is_object()) {
            let channel = if schedule["channel"] == "feishu" {
                "飞书"
            } else {
                "网页"
            };
            let status = match schedule["status"].as_str() {
                Some("enabled") => "已启用",
                Some("paused") => "已暂停",
                Some("ended") => "已结束",
                _ => "已保存",
            };
            reply.push_str(&format!(
                "\n- 「{}」：{}投递，{}",
                schedule["title"].as_str().unwrap_or(""),
                channel,
                status
            ));
            if status == "已启用" {
                let time = schedule["next_run_at"].as_str().unwrap_or("");
                let zone = schedule["timezone"].as_str().unwrap_or("UTC");
                let local = chrono::DateTime::parse_from_rfc3339(time)
                    .ok()
                    .zip(zone.parse::<chrono_tz::Tz>().ok())
                    .map(|(time, zone)| {
                        time.with_timezone(&zone)
                            .format("%Y-%m-%d %H:%M:%S")
                            .to_string()
                    })
                    .unwrap_or_else(|| time.to_owned());
                reply.push_str(&format!("，下次执行：{local}（{zone}）"));
            }
            reply.push('。');
        } else {
            reply.push_str("\n- 另有一项待办变更已保存。");
        }
    }
    if results.len() > MAX_RECEIPTS as usize {
        reply.push_str("\n- 还有已提交的操作，请在待办详情查看。");
    }
    reply.push_str("\n\n以上为本次提交记录；其他步骤是否完成尚未确认，当前状态可在待办详情查看。");
    Ok(Some(reply))
}
