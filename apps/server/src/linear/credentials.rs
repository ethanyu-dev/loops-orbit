use super::{configured, unavailable};
use crate::{AppState, error::ApiResult};
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, Generate, KeyInit, Payload},
};
use serde::{Deserialize, Serialize};
// GCM 使用随机 96 位 nonce，密文绑定当前 OAuth 应用和管理员身份。
const NONCE_BYTES: usize = 12;
/// 明文令牌只在授权交换、刷新和请求期间存在于内存。
#[derive(Serialize, Deserialize)]
pub(super) struct Tokens {
    /// 短期访问令牌。
    pub access_token: String,
    /// 轮换刷新令牌。
    pub refresh_token: String,
}
/// 应用间不能复用密文，避免配置错误导致串用凭证。
fn aad(state: &AppState) -> ApiResult<String> {
    Ok(format!(
        "orbit:linear:admin:{}",
        configured(state)?.client_id
    ))
}
/// 每次落盘使用新的 nonce，不将凭证放入任何响应。
pub(super) fn seal(state: &AppState, tokens: &Tokens) -> ApiResult<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(&configured(state)?.token_key).map_err(unavailable)?;
    let nonce = Nonce::<aes_gcm::aead::consts::U12>::generate();
    let encrypted = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: &serde_json::to_vec(tokens).map_err(unavailable)?,
                aad: aad(state)?.as_bytes(),
            },
        )
        .map_err(unavailable)?;
    Ok([nonce.as_slice(), encrypted.as_slice()].concat())
}
/// 篡改、截断和应用不匹配一律拒绝，不回退其他账号凭证。
pub(super) fn open(state: &AppState, bytes: &[u8]) -> ApiResult<Tokens> {
    let cipher = Aes256Gcm::new_from_slice(&configured(state)?.token_key).map_err(unavailable)?;
    let nonce = Nonce::try_from(
        bytes
            .get(..NONCE_BYTES)
            .ok_or_else(|| unavailable("凭证损坏"))?,
    )
    .map_err(unavailable)?;
    let plain = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &bytes[NONCE_BYTES..],
                aad: aad(state)?.as_bytes(),
            },
        )
        .map_err(unavailable)?;
    serde_json::from_slice(&plain).map_err(unavailable)
}
