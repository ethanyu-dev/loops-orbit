use super::{Document, LOCAL_TIMEZONE, search, store};
use crate::{
    AppState, auth,
    error::{ApiError, ApiResult},
};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

// 页大小和正文预算分别限制记录数与模型输入，不截断后假装完整读取。
const DEFAULT_PAGE_SIZE: usize = 20;
const MAX_PAGE_SIZE: usize = 50;
const PAGE_BYTES: usize = 24000;
const TEXT_PART_CHARS: usize = 2000;
const MAX_QUERY_CHARS: usize = 200;
const LABEL_CHARS: usize = 160;
const DOCUMENTS_SQL: &str = include_str!("../sql/communication_tool_documents.sql");

/// 分页同时绑定快照；资料变更时拒绝续页，避免偏移产生漏项。
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    /// 从零开始的条目偏移。
    #[serde(default)]
    offset: usize,
    /// 每页请求数量，还会受字节预算约束。
    limit: Option<usize>,
    /// 服务端上一页返回的指纹，第一页可省略。
    snapshot: Option<String>,
}
impl Page {
    /// 数量、游标和快照必须共同有效，不能静默接受过期页码。
    fn validate(&self, total: usize, snapshot: &str) -> ApiResult<usize> {
        let limit = self.limit.unwrap_or(DEFAULT_PAGE_SIZE);
        if limit == 0 || limit > MAX_PAGE_SIZE {
            return Err(invalid());
        }
        if (self.offset > 0 && self.snapshot.is_none())
            || self
                .snapshot
                .as_deref()
                .is_some_and(|value| value != snapshot)
        {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "communication_snapshot_changed",
            ));
        }
        if self.offset > total {
            return Err(invalid());
        }
        Ok(limit)
    }
}

/// 查询参数仅描述过滤条件，身份和权限不能由模型指定。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    /// 北京时间起始日，包含当天。
    start_day: Option<String>,
    /// 北京时间结束日，包含当天。
    end_day: Option<String>,
    /// 可选字面关键词；空值表示按日期完整列举。
    #[serde(default)]
    query: String,
    /// 可选来源，必须仍属于当前授权账号。
    source_id: Option<Uuid>,
    /// 分页独立反序列化，避免 flatten 与未知字段校验的冲突。
    #[serde(default)]
    offset: usize,
    /// 每页资料数量上限。
    limit: Option<usize>,
    /// 续页需要的范围和内容指纹。
    snapshot: Option<String>,
}

/// 文档与来源状态来自同一 SQL 快照，目录结果不暴露磁盘指纹。
#[derive(Serialize, sqlx::FromRow)]
struct Record {
    /// 日文件及原文、摘要版本。
    #[sqlx(flatten)]
    document: Document,
    /// 展示名称属于不可信资料。
    label: String,
    /// 暂停或移除来源不参与工具查询。
    enabled: bool,
    /// 订阅状态纳入分页快照，变更时要求重查。
    subscribed: bool,
}

/// 读取请求必须携带列表返回的版本；默认读取摘要，可显式获取原文。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    /// 查询结果的真实文档标识。
    document_id: Uuid,
    /// 原文版本围栏，拒绝用旧文档位置读取新内容。
    version: i64,
    /// summary 或 messages，省略时优先使用摘要。
    mode: Option<String>,
    /// 条目或长消息分段的偏移。
    #[serde(default)]
    offset: usize,
    /// 每页条目上限。
    limit: Option<usize>,
    /// 上次读取的内容指纹。
    snapshot: Option<String>,
}

/// 参数错误使用统一分类，不暴露底层反序列化细节。
fn invalid() -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, "invalid_arguments")
}

/// 日期必须是规范的有效自然日，字符串比较才与日历顺序一致。
fn validate_day(value: Option<&str>) -> ApiResult<()> {
    if let Some(value) = value {
        let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| invalid())?;
        if value.len() != 10 || date.format("%Y-%m-%d").to_string() != value {
            return Err(invalid());
        }
    }
    Ok(())
}

/// 两个只读入口共用锁、权限检查和固定 SQL，不向模型开放任意查询。
pub(crate) async fn execute(
    state: &AppState,
    owner: &str,
    name: &str,
    args: Value,
) -> ApiResult<Value> {
    let _guard = state.communications.lock().await;
    if !search::allowed(state, owner).await? {
        return Err(ApiError(StatusCode::FORBIDDEN, "communication_forbidden"));
    }
    match name {
        "communication_search" => {
            list(
                state,
                owner,
                serde_json::from_value(args).map_err(|_| invalid())?,
            )
            .await
        }
        "communication_read" => {
            read(
                state,
                owner,
                serde_json::from_value(args).map_err(|_| invalid())?,
            )
            .await
        }
        _ => Err(invalid()),
    }
}

/// 查询元数据用于日期完整列举；关键词仅在指定范围内读取文件，不依赖向量就绪。
async fn list(state: &AppState, owner: &str, input: Search) -> ApiResult<Value> {
    validate_day(input.start_day.as_deref())?;
    validate_day(input.end_day.as_deref())?;
    if input
        .start_day
        .as_ref()
        .zip(input.end_day.as_ref())
        .is_some_and(|(start, end)| start > end)
        || input.query.chars().count() > MAX_QUERY_CHARS
    {
        return Err(invalid());
    }
    let rows: Vec<Record> = sqlx::query_as(DOCUMENTS_SQL)
        .bind(owner)
        .bind(&input.start_day)
        .bind(&input.end_day)
        .bind(input.source_id)
        .bind(None::<Uuid>)
        .fetch_all(&state.pool)
        .await?;
    let query = input.query.trim().to_lowercase();
    let mut matches = Vec::new();
    let (mut excluded, mut pending, mut unreadable) = (0, 0, 0);
    for row in &rows {
        if !row.enabled {
            excluded += 1;
            continue;
        }
        if row.document.extraction_version != 1 {
            pending += 1;
            continue;
        }
        if !query.is_empty() && !row.label.to_lowercase().contains(&query) {
            let Ok(raw) = store::raw(state, &row.document) else {
                unreadable += 1;
                continue;
            };
            let raw_match = raw
                .iter()
                .any(|message| !message.deleted && message.text.to_lowercase().contains(&query));
            let summary_match = store::summary(state, &row.document)
                .ok()
                .is_some_and(|summary| {
                    summary
                        .items
                        .iter()
                        .any(|item| item.text.to_lowercase().contains(&query))
                });
            let image_match = super::images::notes(state, &row.document)
                .await?
                .iter()
                .any(|note| {
                    note.description
                        .as_ref()
                        .is_some_and(|text| text.to_lowercase().contains(&query))
                });
            if !raw_match && !summary_match && !image_match {
                continue;
            }
        }
        matches.push(metadata(row));
    }
    let scope = json!({"start_day":input.start_day,"end_day":input.end_day,"timezone":LOCAL_TIMEZONE.name(),"query":query,"source_id":input.source_id,"source_policy":"enabled_only"});
    let snapshot = auth::hash(&json!([scope, rows, matches, unreadable]).to_string());
    let page = Page {
        offset: input.offset,
        limit: input.limit,
        snapshot: input.snapshot,
    };
    let limit = page.validate(matches.len(), &snapshot)?;
    let mut result = paginate(matches, &page, limit, &snapshot);
    result["scope"] = scope;
    result["coverage"] = json!({"excluded_inactive_count":excluded,"pending_count":pending,"unreadable_count":unreadable,"keyword_scan_complete":unreadable==0,"raw_files_verified":false});
    Ok(result)
}

/// 元数据保留读取所需标识、版本和可读出处，不向模型提供会话跳转地址。
fn metadata(row: &Record) -> Value {
    json!({"document_id":row.document.id,"source_id":row.document.source_id,"version":row.document.version,"day":row.document.day,"source":short(&row.label),"summary_available":row.document.summary_hash.is_some(),"summary_status":row.document.summary_status})
}

/// 展示名也受长度限制，避免不可信名称突破正文预算。
fn short(value: &str) -> String {
    value.chars().take(LABEL_CHARS).collect()
}

/// 按条目和字节双重分页，每个条目必须能独立放入一页。
fn paginate(items: Vec<Value>, page: &Page, limit: usize, snapshot: &str) -> Value {
    let total = items.len();
    let (mut selected, mut bytes) = (Vec::new(), 0);
    for item in items.into_iter().skip(page.offset).take(limit) {
        let size = item.to_string().len();
        if bytes + size > PAGE_BYTES {
            break;
        }
        bytes += size;
        selected.push(item);
    }
    let next = page.offset + selected.len();
    json!({"items":selected,"total":total,"offset":page.offset,"next_offset":(next<total).then_some(next),"has_more":next<total,"snapshot":snapshot})
}

/// 长消息按 Unicode 字符拆分，分页能继续读取全文，不把截断当作已读完。
fn text_parts(base: Value, text: &str, output: &mut Vec<Value>) {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len().div_ceil(TEXT_PART_CHARS).max(1);
    for index in 0..total {
        let start = index * TEXT_PART_CHARS;
        let end = ((index + 1) * TEXT_PART_CHARS).min(chars.len());
        let mut item = base.clone();
        item["text"] = json!(chars[start..end].iter().collect::<String>());
        item["text_part"] = json!(index + 1);
        item["text_parts"] = json!(total);
        output.push(item);
    }
}

/// 读取当前文件并验证哈希、版本和摘要证据；损坏文件明确报错，不退回旧数据库正文。
async fn read(state: &AppState, owner: &str, input: Read) -> ApiResult<Value> {
    let mode = input.mode.as_deref().unwrap_or("summary");
    if !matches!(mode, "summary" | "messages") || input.version < 1 {
        return Err(invalid());
    }
    let row: Record = sqlx::query_as(DOCUMENTS_SQL)
        .bind(owner)
        .bind(None::<String>)
        .bind(None::<String>)
        .bind(None::<Uuid>)
        .bind(input.document_id)
        .fetch_optional(&state.pool)
        .await?
        .filter(|row: &Record| row.enabled && row.document.extraction_version == 1)
        .ok_or(ApiError(StatusCode::NOT_FOUND, "communication_not_found"))?;
    let doc = &row.document;
    if doc.version != input.version {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "communication_source_changed",
        ));
    }
    let raw = store::raw(state, doc)?;
    let summary = if mode == "summary" {
        store::summary(state, doc)
            .ok()
            .filter(|summary| !summary.items.is_empty())
    } else {
        None
    };
    let actual_mode = if summary.is_some() {
        "summary"
    } else {
        "messages"
    };
    let mut items = vec![];
    let time = |millis| {
        chrono::DateTime::from_timestamp_millis(millis)
            .map(|t| t.with_timezone(&LOCAL_TIMEZONE).to_rfc3339())
    };
    if let Some(summary) = &summary {
        for (index, item) in summary.items.iter().enumerate() {
            let sender = raw
                .iter()
                .find(|message| message.message_id == item.message_id)
                .map(|message| short(message.display_name()));
            items.push(json!({"kind":item.kind,"item":index,"text":item.text,"quote":item.quote,"sender":sender,"is_me":item.is_me,"time":time(item.create_time)}));
        }
    } else {
        for (index, message) in raw.iter().filter(|message| !message.deleted).enumerate() {
            text_parts(
                json!({"kind":"message","message_index":index,"sender":short(message.display_name()),"is_me":message.is_me,"time":time(message.create_time),"message_type":short(&message.message_type)}),
                &message.text,
                &mut items,
            );
        }
    }
    let mut notes = super::images::notes(state, doc).await?;
    notes.sort_by(|a, b| (&a.message_id, &a.image_key).cmp(&(&b.message_id, &b.image_key)));
    for (index, note) in notes.iter().enumerate() {
        let message = raw
            .iter()
            .find(|message| message.message_id == note.message_id);
        text_parts(
            json!({"kind":"image_interpretation","image_index":index,"sender":message.map(|m|short(m.display_name())),"is_me":message.is_some_and(|m|m.is_me),"time":message.and_then(|m|time(m.create_time)),"error":note.error}),
            note.description.as_deref().unwrap_or(""),
            &mut items,
        );
    }
    let snapshot = auth::hash(&json!([doc, actual_mode, items]).to_string());
    let page = Page {
        offset: input.offset,
        limit: input.limit,
        snapshot: input.snapshot,
    };
    let limit = page.validate(items.len(), &snapshot)?;
    let mut result = paginate(items, &page, limit, &snapshot);
    result["document"] = metadata(&row);
    result["mode"] = json!(actual_mode);
    result["summary_fallback"] = json!(mode == "summary" && summary.is_none());
    result["coverage"] = json!({"summary_status":doc.summary_status,"rejected_count":summary.as_ref().map(|s|s.rejected_count),"failed_chunk_count":summary.as_ref().map(|s|s.failed_chunk_count),"message_count":raw.iter().filter(|m|!m.deleted).count(),"unsupported_count":summary.as_ref().map(|s|s.unsupported_count),"non_text_count":raw.iter().filter(|m|!m.deleted && m.text.trim().is_empty()).count(),"images_unavailable":notes.iter().filter(|n|n.description.is_none()).count()});
    Ok(result)
}
