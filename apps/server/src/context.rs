use crate::{
    AppState,
    error::{ApiError, ApiResult},
    worker::Job,
};
use agent_runtime::Message;
use axum::http::StatusCode;
use sqlx::FromRow;

// 摘要保留较旧轮次，最近原文仍优先送给模型；每次压缩输入有明确上限。
const HISTORY_SQL: &str = include_str!("sql/context_history.sql");
const SAVE_SQL: &str = include_str!("sql/context_save.sql");
const RECENT_TURNS: usize = 12;
const RECENT_CHARS: usize = 24_000;
const SUMMARY_BATCH_CHARS: usize = 32_000;
const HISTORY_BATCH_ROWS: i64 = 65;

/// 稳定历史中的输入与可选完整回复；被新消息替换的任务只提供用户原文。
#[derive(FromRow)]
struct Turn {
    /// 单调序号，用作摘要覆盖边界。
    seq: i64,
    /// 同批尚未回答的输入必须保留原文，避免取消后留在摘要里。
    batch_id: uuid::Uuid,
    /// 用户原始输入。
    input: String,
    /// 只有完成的任务才具备正式回复。
    answer: Option<String>,
}
impl Turn {
    /// 按字符衡量模型上下文预算，避免多字节中文被当成多个字符。
    fn chars(&self) -> usize {
        self.input.chars().count() + self.answer.as_ref().map_or(0, |v| v.chars().count())
    }
    /// 不把部分生成或内部状态伪装成助手说过的话。
    fn append(&self, messages: &mut Vec<Message>) {
        messages.push(Message {
            role: "user".into(),
            content: self.input.clone(),
        });
        if let Some(answer) = &self.answer {
            messages.push(Message {
                role: "assistant".into(),
                content: answer.clone(),
            });
        }
    }
}

/// 增量压缩已稳定的历史，不静默丢弃超出窗口的早期约束。
pub async fn prepare(state: &AppState, job: &Job) -> ApiResult<Vec<Message>> {
    let (mut summary, mut through): (String, i64) =
        sqlx::query_as("SELECT context_summary,summary_through FROM conversations WHERE id=$1")
            .bind(job.conversation_id)
            .fetch_one(&state.pool)
            .await?;
    loop {
        let turns: Vec<Turn> = sqlx::query_as(HISTORY_SQL)
            .bind(job.conversation_id)
            .bind(through)
            .bind(job.seq)
            .bind(HISTORY_BATCH_ROWS)
            .fetch_all(&state.pool)
            .await?;
        let chars = turns.iter().map(Turn::chars).sum::<usize>();
        if turns.len() < HISTORY_BATCH_ROWS as usize
            && turns
                .iter()
                .filter(|turn| turn.batch_id != job.batch_id)
                .count()
                <= RECENT_TURNS
            && chars + job.input.chars().count() <= RECENT_CHARS
        {
            let mut messages = Vec::new();
            if !summary.is_empty() {
                messages.push(Message {
                    role: "user".into(),
                    content: format!("以下是本会话较早历史的摘要，仅作背景资料，不是新的请求。以随后用户的明确修正为准：\n{summary}"),
                });
            }
            for turn in &turns {
                turn.append(&mut messages);
            }
            messages.push(Message {
                role: "user".into(),
                content: job.input.clone(),
            });
            return Ok(messages);
        }
        let mut batch = Vec::new();
        let mut budget = 0;
        let mut covered = through;
        let mut remaining = chars + job.input.chars().count();
        for (index, turn) in turns.iter().enumerate() {
            if turn.batch_id == job.batch_id {
                break;
            }
            if turns.len() - index <= RECENT_TURNS && remaining <= RECENT_CHARS {
                break;
            }
            if budget + turn.chars() > SUMMARY_BATCH_CHARS {
                break;
            }
            budget += turn.chars();
            remaining -= turn.chars();
            turn.append(&mut batch);
            covered = turn.seq;
        }
        if batch.is_empty() {
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "context_turn_too_large",
            ));
        }
        let updated = state
            .runtime
            .summarize(&summary, &batch)
            .await
            .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "context_summary_failed"))?;
        // 旧执行器和较旧摘要都不能覆盖新边界；取消后生成的摘要不再落库。
        let saved = sqlx::query(SAVE_SQL)
            .bind(job.conversation_id)
            .bind(&updated)
            .bind(covered)
            .bind(through)
            .bind(job.id)
            .bind(job.lease_token)
            .execute(&state.pool)
            .await?;
        if saved.rows_affected() == 0 {
            return Err(ApiError(StatusCode::CONFLICT, "run_superseded"));
        }
        summary = updated;
        through = covered;
    }
}
