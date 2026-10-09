mod decision;
mod evidence;
mod rules;
mod settings;
mod worker;

pub use decision::Config;
pub(super) use settings::{read, save};
pub(super) use worker::enqueue;
pub use worker::step;

// 只在显式开启接管时缩短私聊轮询；非实时事件，实际延迟还取决于积压。
pub(super) const POLL_SECONDS: i64 = 15;
// 标识在发送前由服务端添加，不能让模型决定是否保留。
const AGENT_PREFIX: &str = "[agent] ";
// 过期消息保持静默，防止长时间停机后突然批量补发。
const MAX_AGE_MS: i64 = 300_000;
