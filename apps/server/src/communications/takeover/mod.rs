mod decision;
mod evidence;
mod rules;
mod self_chat;
mod settings;
mod worker;

pub use decision::Config;
pub(super) use settings::{read, save};
pub(super) use worker::enqueue;
pub use worker::step;

// 只在显式开启接管时缩短私聊轮询；非实时事件，实际延迟还取决于积压。
pub(super) const POLL_SECONDS: i64 = 15;
// 标识单独占一行，由服务端添加，不能让模型决定是否保留。
const AGENT_PREFIX: &str = "[Agent 自动回复]\n";
// 过期消息保持静默，防止长时间停机后突然批量补发。
const MAX_AGE_MS: i64 = 300_000;

/// 接管与知识整理共用标识判断；保留旧前缀兼容，避免历史回复重放后形成循环。
pub(crate) fn is_agent_message(text: &str) -> bool {
    let text = text.trim_start();
    text.starts_with(AGENT_PREFIX.trim_end()) || text.starts_with("[agent]")
}
