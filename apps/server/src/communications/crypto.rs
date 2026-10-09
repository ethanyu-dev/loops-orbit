use super::{configured, unavailable};
use crate::{AppState, error::ApiResult};
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, Generate, KeyInit, Payload},
};
use serde::{Deserialize, Serialize};

// GCM 使用 96 位随机 nonce；认证标签由库生成、校验。
const NONCE_BYTES: usize = 12;
/// 仅加密前后存在于服务端内存中的令牌对。
#[derive(Serialize, Deserialize)]
pub(super) struct Tokens {
    /// 用户访问令牌。
    pub access_token: String,
    /// 可轮换的刷新令牌。
    pub refresh_token: String,
    /// 飞书实际授予范围；旧密文默认无发送授权。
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
}
/// 将密文绑定到应用和唯一数据所有者，不能移植到另一应用配置。
fn aad(state: &AppState) -> String {
    format!(
        "orbit:communications:admin:{}",
        state.config.feishu.as_ref().expect("已验证飞书配置").app_id
    )
}
/// 每次写入生成新的 nonce，明文与认证标签均交由标准 AEAD 库处理。
pub(super) fn seal(state: &AppState, tokens: &Tokens) -> ApiResult<Vec<u8>> {
    let cipher = Aes256Gcm::new_from_slice(&configured(state)?.token_key).map_err(unavailable)?;
    let nonce = Nonce::<aes_gcm::aead::consts::U12>::generate();
    let bytes = serde_json::to_vec(tokens).map_err(unavailable)?;
    let encrypted = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: &bytes,
                aad: aad(state).as_bytes(),
            },
        )
        .map_err(unavailable)?;
    Ok([nonce.as_slice(), encrypted.as_slice()].concat())
}
/// 校验失败不尝试解读明文，也不在日志输出密文或密钥。
pub(super) fn open(state: &AppState, bytes: &[u8]) -> ApiResult<Tokens> {
    let cipher = Aes256Gcm::new_from_slice(&configured(state)?.token_key).map_err(unavailable)?;
    let nonce = bytes
        .get(..NONCE_BYTES)
        .ok_or_else(|| unavailable("凭证损坏"))?;
    let nonce = Nonce::try_from(nonce).map_err(unavailable)?;
    let plain = cipher
        .decrypt(
            &nonce,
            Payload {
                msg: &bytes[NONCE_BYTES..],
                aad: aad(state).as_bytes(),
            },
        )
        .map_err(unavailable)?;
    serde_json::from_slice(&plain).map_err(unavailable)
}
