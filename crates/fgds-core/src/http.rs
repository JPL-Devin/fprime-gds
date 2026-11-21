use crate::{Error, SendError};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// The error body of every route: `{"error": {"code": ..., "message": ...}}` (API-7).
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError { status, code, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        ApiError::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({"error": {"code": self.code, "message": self.message}});
        (self.status, Json(body)).into_response()
    }
}

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        match e {
            Error::Invalid(message) => ApiError::bad_request(message),
            other => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", other.to_string()),
        }
    }
}

/// The status of each send outcome is fixed by section 4.5 of the design.
impl From<SendError> for ApiError {
    fn from(e: SendError) -> Self {
        let (status, code) = match e {
            SendError::UnknownKind(_) => (StatusCode::NOT_FOUND, "unknown_kind"),
            SendError::Invalid(_) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid"),
            SendError::LinkDown => (StatusCode::SERVICE_UNAVAILABLE, "link_down"),
            SendError::Failed(_) => (StatusCode::BAD_GATEWAY, "send_failed"),
        };
        ApiError::new(status, code, e.to_string())
    }
}
