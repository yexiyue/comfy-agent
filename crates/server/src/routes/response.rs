//! Convert transport and store failures into safe HTTP responses.
use axum::{
    Json,
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use runtime::store::{StoreError, StoreResult};
use serde_json::json;
pub(crate) fn error(error: StoreError) -> Response {
    let status = match &error {
        StoreError::NotFound => StatusCode::NOT_FOUND,
        StoreError::Conflict => StatusCode::CONFLICT,
        StoreError::Invalid(_) => StatusCode::BAD_REQUEST,
        StoreError::Unavailable(cause) => {
            tracing::error!(%cause,"storage request failed");
            StatusCode::SERVICE_UNAVAILABLE
        }
    };
    (status, Json(json!({"error":error.to_string()}))).into_response()
}
pub(super) fn input<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, Box<Response>> {
    value.map(|Json(v)| v).map_err(|e| {
        (
            if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                e.status()
            } else {
                StatusCode::BAD_REQUEST
            },
            Json(json!({"error":e.body_text()})),
        )
            .into_response()
            .into()
    })
}
pub(super) fn response<T: serde::Serialize>(value: StoreResult<T>) -> Response {
    match value {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(e),
    }
}
