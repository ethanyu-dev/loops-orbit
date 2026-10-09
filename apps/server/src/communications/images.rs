use super::{DOCUMENT_COLUMNS, Document, client, store, unavailable};
use crate::{
    AppState, auth,
    auth::Identity,
    error::{ApiError, ApiResult},
};
use axum::{
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// 图片按需读取，不持久化带凭证的 URL；限制单张资源大小和后台处理速率。
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMAGES_PER_MESSAGE: usize = 20;

/// 图片解读独立于原话，不用于伪造逐字引用或自动确认承诺。
#[derive(Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Note {
    /// 原消息标识，仅用于内部关联与资源路由。
    pub message_id: String,
    /// 消息中的图片键，不含用户授权令牌。
    pub image_key: String,
    /// 模型解读，不能称为发送者的原话。
    pub description: Option<String>,
    /// 稳定错误分类，失败不影响文字同步。
    pub error: Option<String>,
}
/// 数据库缓存携带指纹，返回资料前必须和当前原文逐条匹配。
#[derive(sqlx::FromRow)]
struct CachedNote {
    /// 消息身份。
    message_id: String,
    /// 飞书图片键。
    image_key: String,
    /// 图片所属原文版本。
    fingerprint: String,
    /// 图片解读。
    description: Option<String>,
    /// 最近失败分类。
    error: Option<String>,
}
/// 原文编辑、解析修正或撤回后不能复用旧图片解读，避免解读继续引用修正前的重复文字。
pub(super) fn fingerprint(message: &store::Message) -> String {
    let mut evidence = format!(
        "{}:{}:{}",
        message.update_time, message.deleted, message.payload
    );
    // 仅富文本附带解析正文；普通图片维持原指纹，避免升级时无关解读全部重算。
    if message.message_type == "post" {
        evidence.push(':');
        evidence.push_str(&message.text);
    }
    auth::hash(&evidence)
}
/// 只提取飞书原生图片节点；不跟随消息正文中的任意图片 URL。
pub(super) fn keys(message: &store::Message) -> Vec<String> {
    fn walk(v: &Value, out: &mut Vec<String>) {
        if let Some(key) = v.get("image_key").and_then(Value::as_str)
            && key.len() <= 256
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            && !out.iter().any(|k| k == key)
        {
            out.push(key.into());
        }
        match v {
            Value::Array(v) => {
                for item in v {
                    walk(item, out)
                }
            }
            Value::Object(v) => {
                for item in v.values() {
                    walk(item, out)
                }
            }
            _ => {}
        }
    }
    let mut out = vec![];
    if !message.deleted
        && matches!(message.message_type.as_str(), "image" | "post")
        && let Some(content) = message.payload["body"]["content"].as_str()
        && let Ok(body) = serde_json::from_str::<Value>(content)
    {
        if message.message_type == "post" {
            if let Some(body) = super::post::body(&body) {
                walk(&body["content"], &mut out);
            }
        } else {
            walk(&body, &mut out);
        }
    }
    out
}
/// 资源地址由受信任 API 根与已校验消息标识构造；禁止任意 URL 代理。
async fn download(
    state: &AppState,
    message: &store::Message,
    key: &str,
) -> ApiResult<(String, Vec<u8>)> {
    if !message
        .message_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || !keys(message).iter().any(|k| k == key)
    {
        return Err(unavailable("无效图片引用"));
    }
    let token = client::access(state).await?;
    let mut response = client::get(
        state,
        &format!("/im/v1/messages/{}/resources/{key}", message.message_id),
        &token,
    )
    .query(&[("type", "image")])
    .timeout(std::time::Duration::from_secs(20))
    .send()
    .await
    .map_err(|error| {
        ApiError(
            StatusCode::BAD_GATEWAY,
            if error.is_timeout() {
                "communication_image_download_timeout"
            } else {
                "communication_image_download_failed"
            },
        )
    })?;
    if !response.status().is_success() {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            match response.status().as_u16() {
                401 | 403 => "communication_image_forbidden",
                404 | 410 => "communication_image_not_found",
                429 => "communication_image_rate_limited",
                _ => "communication_image_unavailable",
            },
        ));
    }
    let mut bytes = vec![];
    while let Some(chunk) = response.chunk().await.map_err(|_| {
        ApiError(
            StatusCode::BAD_GATEWAY,
            "communication_image_download_failed",
        )
    })? {
        if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
            return Err(ApiError(
                StatusCode::BAD_GATEWAY,
                "communication_image_too_large",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    // 飞书可能返回 application/octet-stream，使用文件签名识别允许的图片格式。
    let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "communication_image_format",
        ));
    };
    Ok((mime.into(), bytes))
}
/// 浏览器图片始终经过管理员会话鉴权，不能把此私有 URL 当公开模型输入。
pub(super) async fn resource(
    State(state): State<AppState>,
    identity: Identity,
    Path((id, message_id, index)): Path<(Uuid, String, usize)>,
) -> ApiResult<Response> {
    identity.require_admin()?;
    let (doc, message, key) = {
        let _guard = state.communications.lock().await;
        let doc: Document=sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {DOCUMENT_COLUMNS} FROM communication_documents WHERE id=$1 AND source_id IN(SELECT id FROM communication_sources WHERE enabled)"))).bind(id).fetch_optional(&state.pool).await?.ok_or(ApiError(StatusCode::NOT_FOUND,"communication_not_found"))?;
        let message = store::raw(&state, &doc)?
            .into_iter()
            .find(|m| m.message_id == message_id && !m.deleted)
            .ok_or(ApiError(StatusCode::NOT_FOUND, "communication_not_found"))?;
        let key = keys(&message)
            .get(index)
            .cloned()
            .ok_or(ApiError(StatusCode::NOT_FOUND, "communication_not_found"))?;
        (doc, message, key)
    };
    let (mime, bytes) = download(&state, &message, &key).await?;
    let _guard = state.communications.lock().await;
    if !current(&state, &doc).await? {
        return Err(ApiError(StatusCode::NOT_FOUND, "communication_not_found"));
    }
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "private, no-store".into()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".into()),
        ],
        bytes,
    )
        .into_response())
}
/// 网络期间发生暂停、遗忘或版本变更时丢弃在途结果。
async fn current(state: &AppState, doc: &Document) -> ApiResult<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_documents d JOIN communication_sources s ON s.id=d.source_id WHERE d.id=$1 AND d.version=$2 AND s.enabled AND EXISTS(SELECT 1 FROM communication_connections WHERE status='active'))").bind(doc.id).bind(doc.version).fetch_one(&state.pool).await?)
}
/// 加载与当前原文匹配的图片解读，失败项同样可显示并等待重试。
pub(super) async fn notes(state: &AppState, doc: &Document) -> ApiResult<Vec<Note>> {
    let raw = store::raw(state, doc)?;
    let rows:Vec<CachedNote>=sqlx::query_as("SELECT message_id,image_key,fingerprint,description,error FROM communication_images WHERE source_id=$1 AND message_id=ANY($2)").bind(doc.source_id).bind(raw.iter().map(|m|m.message_id.clone()).collect::<Vec<_>>()).fetch_all(&state.pool).await?;
    Ok(rows
        .into_iter()
        .filter_map(
            |CachedNote {
                 message_id,
                 image_key,
                 fingerprint: hash,
                 description,
                 error,
             }| {
                raw.iter()
                    .any(|m| {
                        m.message_id == message_id
                            && !m.deleted
                            && fingerprint(m) == hash
                            && keys(m).contains(&image_key)
                    })
                    .then_some(Note {
                        message_id,
                        image_key,
                        description,
                        error,
                    })
            },
        )
        .collect())
}
/// 每步仅识别一张图片；失败五分钟后重试，成功结果按原图版本复用。
pub(super) async fn step(state: &AppState) -> ApiResult<()> {
    for doc in super::search::documents(state).await? {
        let raw = {
            let _guard = state.communications.lock().await;
            match store::raw(state, &doc) {
                Ok(raw) => raw,
                Err(_) => continue,
            }
        };
        for message in &raw {
            for key in keys(message).into_iter().take(MAX_IMAGES_PER_MESSAGE) {
                let hash = fingerprint(message);
                let skip:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM communication_images WHERE source_id=$1 AND message_id=$2 AND image_key=$3 AND fingerprint=$4 AND (description IS NOT NULL OR next_attempt>now()))").bind(doc.source_id).bind(&message.message_id).bind(&key).bind(&hash).fetch_one(&state.pool).await?;
                if skip {
                    continue;
                }
                let result = async {
                    let (mime, bytes) = download(state, message, &key).await?;
                    let data = format!(
                        "data:{mime};base64,{}",
                        base64::engine::general_purpose::STANDARD.encode(bytes)
                    );
                    state
                        .runtime
                        .describe_communication_image(&message.text, &data)
                        .await
                        .map_err(|error| ApiError(StatusCode::BAD_GATEWAY, error.code))
                }
                .await;
                let _guard = state.communications.lock().await;
                if !current(state, &doc).await? {
                    return Ok(());
                }
                let (description, error) = match result {
                    Ok(text) => (Some(text), None),
                    Err(e) => (None, Some(e.1)),
                };
                sqlx::query("INSERT INTO communication_images(source_id,message_id,image_key,fingerprint,description,error,next_attempt) VALUES($1,$2,$3,$4,$5,$6,now()+interval '5 minutes') ON CONFLICT(source_id,message_id,image_key) DO UPDATE SET fingerprint=excluded.fingerprint,description=excluded.description,error=excluded.error,next_attempt=excluded.next_attempt").bind(doc.source_id).bind(&message.message_id).bind(key).bind(hash).bind(&description).bind(error).execute(&state.pool).await?;
                if description.is_some() {
                    // 版本围栏让正在生成的旧摘要失效；原文内容保持不变。
                    super::dependencies::cancel(state, Some(doc.id), None).await?;
                    sqlx::query("UPDATE communication_documents SET version=version+1,summary_hash=NULL,summary_error=NULL,next_summary=now() WHERE id=$1").bind(doc.id).execute(&state.pool).await?;
                    super::search::remove_vector(state, doc.id).await?;
                }
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 验证图片和文字选取同一语言、正文修正使解读失效；不调用模型或验证真实图片内容。
    #[test]
    fn post_images_follow_selected_language_and_text_version() {
        let value = json!({"message_id":"om_fixture","create_time":"1000","msg_type":"post","body":{"content":json!({
            "zh_cn":{"content":[[{"tag":"text","text":"中文"}],[{"tag":"img","image_key":"img_cn"}]]},
            "en_us":{"content":[[{"tag":"text","text":"English"}],[{"tag":"img","image_key":"img_en"}]]}
        }).to_string()}});
        let mut message = super::super::sync::normalize(&value, "oc_fixture", "ou_me").unwrap();
        assert_eq!(keys(&message), vec!["img_cn"]);
        let original = fingerprint(&message);
        message.text = "中文\n中文".into();
        assert_ne!(fingerprint(&message), original);
    }
}
