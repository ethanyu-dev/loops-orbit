use crate::{
    AppState, api, auth,
    config::FeishuConfig,
    error::{ApiError, ApiResult},
};
use aes::cipher::{BlockModeDecrypt, KeyIvInit, block_padding::Pkcs7};
use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::sync::watch;
use uuid::Uuid;

// 飞书签名只接受五分钟窗口，重复事件由数据库唯一键处理。
const SIGNATURE_WINDOW_SECONDS: i64 = 300;
const MAX_DELIVERY_ATTEMPTS: i32 = 5;

// 回复队列领取 SQL 与模型任务独立维护。
const CLAIM_DELIVERY_SQL: &str = include_str!("sql/claim_delivery.sql");

/// 对原始字节验签，再解密，避免 JSON 重序列化改变签名输入。
pub fn decode(headers: &HeaderMap, body: &[u8], key: &str) -> ApiResult<Value> {
    let unauthorized = || ApiError(StatusCode::UNAUTHORIZED, "invalid_feishu_signature");
    let timestamp = headers
        .get("x-lark-request-timestamp")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(unauthorized)?;
    let nonce = headers
        .get("x-lark-request-nonce")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(unauthorized)?;
    let signature = headers
        .get("x-lark-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(unauthorized)?;
    let ts: i64 = timestamp.parse().map_err(|_| unauthorized())?;
    if ts.abs_diff(chrono::Utc::now().timestamp()) > SIGNATURE_WINDOW_SECONDS as u64 {
        return Err(unauthorized());
    }
    let mut digest = Sha256::new();
    digest.update(timestamp);
    digest.update(nonce);
    digest.update(key);
    digest.update(body);
    if !auth::constant_eq(&hex::encode(digest.finalize()), signature) {
        return Err(unauthorized());
    }
    decode_payload(body, key)
}

/// 解密载荷与验签分开，仅 URL challenge 可以走无签名兼容分支。
fn decode_payload(body: &[u8], key: &str) -> ApiResult<Value> {
    let unauthorized = || ApiError(StatusCode::UNAUTHORIZED, "invalid_feishu_payload");
    let data: Value = serde_json::from_slice(body)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_event"))?;
    if let Some(encrypted) = data["encrypt"].as_str() {
        let raw = base64::engine::general_purpose::STANDARD
            .decode(encrypted)
            .map_err(|_| unauthorized())?;
        if raw.len() < 32 {
            return Err(unauthorized());
        }
        let secret = Sha256::digest(key.as_bytes());
        let plain = cbc::Decryptor::<aes::Aes256>::new_from_slices(&secret, &raw[..16])
            .map_err(|_| unauthorized())?
            .decrypt_padded_vec::<Pkcs7>(&raw[16..])
            .map_err(|_| unauthorized())?;
        serde_json::from_slice(&plain).map_err(|_| unauthorized())
    } else {
        Ok(data)
    }
}

/// 回调仅完成认证、白名单和持久化入队，模型请求由后台执行。
pub async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let config = state
        .config
        .feishu
        .as_ref()
        .ok_or(ApiError(StatusCode::NOT_FOUND, "feishu_disabled"))?;
    let data = if headers.contains_key("x-lark-signature") {
        decode(&headers, &body, &config.encrypt_key)?
    } else {
        // 官方 challenge 流程可能没有签名头；只允许验证令牌后的回显，不允许入队。
        let candidate = decode_payload(&body, &config.encrypt_key)?;
        if candidate["type"] != "url_verification" {
            return Err(ApiError(
                StatusCode::UNAUTHORIZED,
                "invalid_feishu_signature",
            ));
        }
        candidate
    };
    let token = data
        .pointer("/header/token")
        .or_else(|| data.get("token"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !auth::constant_eq(&auth::hash(token), &auth::hash(&config.verification_token)) {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_feishu_token"));
    }
    if data["type"] == "url_verification" {
        return Ok(Json(json!({"challenge":data["challenge"]})));
    }
    if data.pointer("/header/app_id").and_then(Value::as_str) != Some(config.app_id.as_str()) {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_feishu_app"));
    }
    if data.pointer("/header/event_type").and_then(Value::as_str) != Some("im.message.receive_v1") {
        return Ok(Json(json!({"ok":true})));
    }
    let event = &data["event"];
    let user = event
        .pointer("/sender/sender_id/open_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    // 首版仅处理白名单用户的单聊文本，避免把私人历史混入群会话。
    if !config.allowed_users.iter().any(|id| id == user)
        || event.pointer("/sender/sender_type").and_then(Value::as_str) != Some("user")
        || event["message"]["chat_type"] != "p2p"
        || event["message"]["message_type"] != "text"
    {
        return Ok(Json(json!({"ok":true})));
    }
    let event_id = data
        .pointer("/header/event_id")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "missing_event_id"))?;
    let message_id = event["message"]["message_id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "missing_message_id"))?;
    let content: Value = serde_json::from_str(event["message"]["content"].as_str().unwrap_or(""))
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_content"))?;
    let text = content["text"].as_str().unwrap_or("").trim();
    if api::validate_input(text).is_err() {
        return Ok(Json(json!({"ok":true})));
    }
    let mut tx = state.pool.begin().await?;
    let inserted = sqlx::query("INSERT INTO feishu_events(id) VALUES($1) ON CONFLICT DO NOTHING")
        .bind(event_id)
        .execute(&mut *tx)
        .await?;
    if inserted.rows_affected() == 0 {
        return Ok(Json(json!({"ok":true})));
    }
    let external = format!("feishu:{user}");
    let conversation: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO conversations (id, owner, channel, external_key, title)
        VALUES ($1, $2, 'feishu', $2, '新对话')
        ON CONFLICT (external_key) DO UPDATE SET external_key = EXCLUDED.external_key
        RETURNING id
    "#,
    )
    .bind(Uuid::new_v4())
    .bind(&external)
    .fetch_one(&mut *tx)
    .await?;
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE conversation_id=$1 AND idempotency_key=$2)",
    )
    .bind(conversation)
    .bind(message_id)
    .fetch_one(&mut *tx)
    .await?;
    if !duplicate {
        let pending: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM runs WHERE conversation_id=$1 AND status IN ('queued','running')",
        )
        .bind(conversation)
        .fetch_one(&mut *tx)
        .await?;
        if pending >= 10 {
            return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "conversation_busy"));
        }
        api::enqueue(&mut tx, conversation, message_id, text, Some(message_id)).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"ok":true})))
}

/// 独立发送队列持久化失败状态；模型结果不会因飞书故障被重新生成。
pub async fn delivery_worker(state: AppState, mut stop: watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            break;
        }
        if let Err(error) = deliver_one(&state).await {
            tracing::warn!(code=%error.1,"飞书发送队列暂时不可用");
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(1)) => {},
            _ = stop.changed() => {},
        }
    }
}

/// 领取后的回复快照，明确区分平台消息 ID 与内部任务 ID。
#[derive(sqlx::FromRow)]
struct Delivery {
    /// 同时作为飞书幂等 UUID 的内部任务 ID。
    id: Uuid,
    /// 飞书被回复消息的 ID。
    reply_to: Option<String>,
    /// 主动发送的明确接收者，与回复旧消息互斥。
    receiver_id: Option<String>,
    /// 可取消的主动事项及对应版本。
    followup_id: Option<Uuid>,
    /// 改期后旧队列不得发送。
    followup_version: Option<i64>,
    /// 已经持久化并截断至平台限制的正文。
    content: String,
    /// 包含本次领取的发送尝试次数。
    attempts: i32,
}

/// 通过 SKIP LOCKED 与租约恢复允许多个实例安全消费回复。
pub async fn deliver_one(state: &AppState) -> ApiResult<()> {
    let config = match &state.config.feishu {
        Some(config) => config,
        None => return Ok(()),
    };
    let lease = Uuid::new_v4();
    let row: Option<Delivery> = sqlx::query_as(CLAIM_DELIVERY_SQL)
        .bind(lease)
        .fetch_optional(&state.pool)
        .await?;
    let Some(delivery) = row else {
        return Ok(());
    };
    // 已经生成但尚未投递的知识答案仍需复核；重试也不能绕过知识撤回或正文变更。
    let uses_knowledge: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM runs WHERE id=$1 AND knowledge_revision IS NOT NULL)",
    )
    .bind(delivery.id)
    .fetch_one(&state.pool)
    .await?;
    let _knowledge_guard = if uses_knowledge {
        let guard = state.communications.lock().await;
        let current:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM runs r,knowledge_state k WHERE r.id=$1 AND r.knowledge_revision=k.revision)")
            .bind(delivery.id).fetch_one(&state.pool).await?;
        if !current {
            sqlx::query("UPDATE outbox SET status='cancelled',lease_until=NULL WHERE id=$1 AND lease_token=$2 AND status='running'")
                .bind(delivery.id).bind(lease).execute(&state.pool).await?;
            return Ok(());
        }
        Some(guard)
    } else {
        None
    };
    let followup = if let Some(id) = delivery.followup_id {
        let Some(job) = crate::followups::scheduler::prepare_delivery(
            state,
            id,
            delivery.followup_version.unwrap_or(0),
        )
        .await?
        else {
            sqlx::query("UPDATE outbox SET status='cancelled',lease_until=NULL WHERE id=$1 AND lease_token=$2 AND status='running'")
                .bind(delivery.id).bind(lease).execute(&state.pool).await?;
            return Ok(());
        };
        if job.owner.strip_prefix("feishu:") != delivery.receiver_id.as_deref() {
            return Err(ApiError(StatusCode::CONFLICT, "followup_owner_unavailable"));
        }
        Some(job)
    } else {
        None
    };
    // 与取消共用会话锁，保证“已经开始投递”的提示不会漏掉并发 HTTP 起点。
    let mut dispatch = state.pool.begin().await?;
    if followup.is_some() {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('orbit:todos:admin'))")
            .execute(&mut *dispatch)
            .await?;
    }
    if let Some(job) = &followup {
        sqlx::query("SELECT id FROM conversations WHERE personal_owner(owner)=personal_owner($1) ORDER BY id FOR UPDATE")
            .bind(&job.owner)
            .execute(&mut *dispatch)
            .await?;
        sqlx::query(
            "SELECT owner FROM followup_preferences WHERE owner=personal_owner($1) FOR UPDATE",
        )
        .bind(&job.owner)
        .execute(&mut *dispatch)
        .await?;
        if !crate::followups::scheduler::can_deliver(state, job, None).await? {
            dispatch.rollback().await?;
            crate::followups::scheduler::prepare_delivery(state, job.id, job.version).await?;
            return Ok(());
        }
    }
    let started = sqlx::query(include_str!("sql/delivery_dispatch_start.sql"))
        .bind(delivery.id)
        .bind(lease)
        .execute(&mut *dispatch)
        .await?;
    if started.rows_affected() == 0 {
        return Ok(());
    }
    dispatch.commit().await?;
    let result = if delivery.attempts > MAX_DELIVERY_ATTEMPTS {
        Err(())
    } else {
        // 发送超时短于租约，避免仍在发送的任务被第二个 worker 领取。
        tokio::time::timeout(
            Duration::from_secs(40),
            send_reply(&state.http, config, &delivery),
        )
        .await
        .unwrap_or(Err(()))
    };
    let status = if result.is_ok() {
        "completed"
    } else if delivery.attempts >= MAX_DELIVERY_ATTEMPTS {
        "failed"
    } else {
        "queued"
    };
    let error = result.err().map(|()| "feishu_delivery_failed");
    let retry_delay = 2_f64.powi(delivery.attempts.min(8));
    let mut tx = state.pool.begin().await?;
    if let Some(job) = &followup {
        sqlx::query("SELECT id FROM conversations WHERE id=$1 FOR UPDATE")
            .bind(job.conversation_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "SELECT owner FROM followup_preferences WHERE owner=personal_owner($1) FOR UPDATE",
        )
        .bind(&job.owner)
        .execute(&mut *tx)
        .await?;
    }
    let updated = sqlx::query(include_str!("sql/delivery_result.sql"))
        .bind(delivery.id)
        .bind(lease)
        .bind(status)
        .bind(error)
        .bind(retry_delay)
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() > 0 {
        if let Some(job) = &followup {
            let current:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM followups WHERE id=$1 AND version=$2 AND status='queued')")
                .bind(job.id).bind(job.version).fetch_one(&mut *tx).await?;
            if current && status == "completed" {
                crate::followups::scheduler::acknowledge(&mut tx, job, &delivery.content).await?;
            } else if current && status == "failed" {
                sqlx::query("UPDATE followups SET status='failed',error='feishu_delivery_failed',updated_at=now() WHERE id=$1 AND version=$2")
                .bind(job.id).bind(job.version).execute(&mut *tx).await?;
            }
        }
    } else if status == "completed" {
        // 即使取消抢先完成，也保留已被外部接受的事实，不能宣称保证撤回。
        sqlx::query("UPDATE outbox SET delivered_at=now() WHERE id=$1")
            .bind(delivery.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    tracing::info!(
        run_id = %delivery.id,
        attempts = delivery.attempts,
        status,
        "飞书回复发送状态",
    );
    Ok(())
}

/// 只负责平台通信，调用者管理租约、总超时及重试状态；错误正文不向外传播。
async fn send_reply(
    http: &reqwest::Client,
    config: &FeishuConfig,
    delivery: &Delivery,
) -> Result<(), ()> {
    let token = tenant_token(http, config).await?;
    let mut url = reqwest::Url::parse(&format!(
        "{}/im/v1/messages",
        config.api_base.trim_end_matches('/')
    ))
    .map_err(|_| ())?;
    if let Some(reply) = &delivery.reply_to {
        url.path_segments_mut()
            .map_err(|_| ())?
            .push(reply)
            .push("reply");
    } else {
        url.query_pairs_mut()
            .append_pair("receive_id_type", "open_id");
    }
    let mut body = json!({"msg_type":"text","content":json!({"text":delivery.content}).to_string(),"uuid":delivery.id.to_string()});
    if let Some(receiver) = &delivery.receiver_id {
        body["receive_id"] = json!(receiver);
    }
    let response: Value = http
        .post(url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|_| ())?
        .error_for_status()
        .map_err(|_| ())?
        .json()
        .await
        .map_err(|_| ())?;
    if response["code"].as_i64() == Some(0) {
        Ok(())
    } else {
        Err(())
    }
}

/// 获取短期平台访问令牌；令牌不持久化且不进入日志。
async fn tenant_token(http: &reqwest::Client, config: &FeishuConfig) -> Result<String, ()> {
    let response: Value = http
        .post(format!(
            "{}/auth/v3/tenant_access_token/internal",
            config.api_base.trim_end_matches('/')
        ))
        .json(&json!({
            "app_id": config.app_id,
            "app_secret": config.app_secret,
        }))
        .send()
        .await
        .map_err(|_| ())?
        .error_for_status()
        .map_err(|_| ())?
        .json()
        .await
        .map_err(|_| ())?;
    if response["code"].as_i64() != Some(0) {
        return Err(());
    }
    response["tenant_access_token"]
        .as_str()
        .map(str::to_owned)
        .ok_or(())
}
