use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

/// 面向 API 的稳定错误，不暴露数据库或供应商的内部信息。
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub &'static str);
pub type ApiResult<T> = Result<T, ApiError>;
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        // 不输出 SQL 参数，保留错误类别便于定位基础设施问题。
        tracing::error!(
            kind = match error {
                sqlx::Error::PoolTimedOut => "pool_timeout",
                sqlx::Error::Database(_) => "database",
                _ => "storage",
            },
            "数据库操作失败"
        );
        Self(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable")
    }
}
