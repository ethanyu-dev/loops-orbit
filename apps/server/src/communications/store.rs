use super::{Document, configured, unavailable};
use crate::{AppState, auth, error::ApiResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use uuid::Uuid;

// 每日文件只保存有界原文；过大时明确失败，不静默丢弃消息。
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// 归一化的消息证据。发送者与“我”由已授权账号标识匹配，不由模型判断。
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    /// 飞书原始 message_id，去重键。
    pub message_id: String,
    /// 原始会话标识。
    pub chat_id: String,
    /// 原始发送者 ID。
    pub sender_id: String,
    /// 上游提供的姓名；缺失时不从 ID 或内容猜测。
    #[serde(default)]
    pub sender_name: String,
    /// 发送者 ID 命名空间，避免 union_id/user_id 与 open_id 混淆。
    pub sender_id_type: String,
    /// user 或 app 等原始发送者类型。
    pub sender_type: String,
    /// 仅 open_id 与授权账号相同且发送者是用户时为真。
    pub is_me: bool,
    /// 飞书毫秒时间戳。
    pub create_time: i64,
    /// 上游更新毫秒时间戳。
    pub update_time: i64,
    /// 原始消息类型。
    pub message_type: String,
    /// 撤回墓碑，无正文。
    pub deleted: bool,
    /// 可供摘要的文本，非文本类型为空。
    pub text: String,
    /// 保留受限原始内容与回复关联；不下载附件。
    pub payload: Value,
}
impl Message {
    /// 正文展示可读身份，原始 ID 仅留作内部证据关联。
    pub fn display_name(&self) -> &str {
        if self.is_me {
            "我"
        } else if self.sender_name.trim().is_empty() {
            "会话成员"
        } else {
            &self.sender_name
        }
    }
}
/// 文档目录只由本地 UUID 构造，不使用上游 chat_id 或文件名。
pub(crate) fn directory(state: &AppState, source: Uuid) -> ApiResult<PathBuf> {
    configured(state)?;
    Ok(state
        .config
        .memory
        .as_ref()
        .expect("已验证配置")
        .directory
        .join("_communications")
        .join(source.to_string()))
}
/// 内容寻址文件在数据库提交前落盘，崩溃不会覆盖当前有效版本。
fn path(state: &AppState, document: &Document, hash: &str, extension: &str) -> ApiResult<PathBuf> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(unavailable("错误文件指纹"));
    }
    Ok(directory(state, document.source_id)?
        .join(document.id.to_string())
        .join(format!("{hash}.{extension}")))
}
/// 不跟随符号链接；哈希不匹配的本地修改不会继续作为已确认资料。
fn read(state: &AppState, document: &Document, hash: &str, extension: &str) -> ApiResult<String> {
    let path = path(state, document, hash, extension)?;
    let metadata = std::fs::symlink_metadata(&path).map_err(unavailable)?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return Err(unavailable("无效原文文件"));
    }
    let text = std::fs::read_to_string(path).map_err(unavailable)?;
    if auth::hash(&text) != hash {
        return Err(unavailable("原文发生变化"));
    }
    Ok(text)
}
/// 读取当前 JSONL 快照；每条消息保留各自发送者和时间。
pub(crate) fn raw(state: &AppState, document: &Document) -> ApiResult<Vec<Message>> {
    read(state, document, &document.raw_hash, "jsonl")?
        .lines()
        .map(|line| serde_json::from_str(line).map_err(unavailable))
        .collect()
}
/// 写入已排序、去重的单日原文，返回用于数据库指针的哈希。
pub(crate) fn write_raw(
    state: &AppState,
    document: &Document,
    messages: &[Message],
) -> ApiResult<String> {
    let mut text = String::new();
    for message in messages {
        text.push_str(&serde_json::to_string(message).map_err(unavailable)?);
        text.push('\n');
    }
    write(state, document, &text, "jsonl")
}
/// 原子写入并 fsync；正文未落盘时绝不推进数据库游标。
fn write(state: &AppState, document: &Document, text: &str, extension: &str) -> ApiResult<String> {
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(unavailable("每日资料过大"));
    }
    let hash = auth::hash(text);
    crate::memory::store::atomic_write(&path(state, document, &hash, extension)?, text.as_bytes())
        .map_err(unavailable)?;
    Ok(hash)
}
/// 摘要第一行是结构化证据，正文是可直接查看的 Markdown。
pub(crate) fn write_summary(
    state: &AppState,
    document: &Document,
    summary: &super::summary::Summary,
) -> ApiResult<String> {
    let mut text = format!(
        "<!-- {} -->\n\n# 沟通整理 · {}\n\n",
        serde_json::to_string(summary).map_err(unavailable)?,
        document.day
    );
    for item in &summary.items {
        text.push_str(&format!(
            "- **{}** {}\n  - 出处：{} · {} · {}\n  - 原话：{}\n",
            item.kind,
            item.text,
            if item.is_me { "我" } else { "会话成员" },
            "查看原始记录",
            item.create_time,
            item.quote
        ));
    }
    write(state, document, &text, "md")
}
/// 同时验证摘要文件和原文文件，避免向量索引孤立保留已遗忘正文。
pub(crate) fn summary(state: &AppState, document: &Document) -> ApiResult<super::summary::Summary> {
    let hash = document
        .summary_hash
        .as_deref()
        .ok_or_else(|| unavailable("尚未完成摘要"))?;
    let text = read(state, document, hash, "md")?;
    let data = text
        .lines()
        .next()
        .and_then(|s| s.strip_prefix("<!-- "))
        .and_then(|s| s.strip_suffix(" -->"))
        .ok_or_else(|| unavailable("无效摘要"))?;
    let summary: super::summary::Summary = serde_json::from_str(data).map_err(unavailable)?;
    if summary.raw_hash != document.raw_hash {
        return Err(unavailable("摘要已过时"));
    }
    let raw = raw(state, document)?;
    super::summary::validate_stored(&summary, &raw)?;
    Ok(summary)
}
/// 提交后删除旧版本，避免撤回正文一直残留在版本文件中。
pub(crate) fn collect(state: &AppState, document: &Document) -> ApiResult<()> {
    let dir = directory(state, document.source_id)?.join(document.id.to_string());
    if !dir.exists() {
        return Ok(());
    }
    let keep_raw = format!("{}.jsonl", document.raw_hash);
    let keep_summary = document
        .summary_hash
        .as_ref()
        .map(|hash| format!("{hash}.md"));
    for entry in std::fs::read_dir(dir).map_err(unavailable)? {
        let entry = entry.map_err(unavailable)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != keep_raw
            && Some(&name) != keep_summary.as_ref()
            && entry.file_type().map_err(unavailable)?.is_file()
        {
            std::fs::remove_file(entry.path()).map_err(unavailable)?;
        }
    }
    Ok(())
}
