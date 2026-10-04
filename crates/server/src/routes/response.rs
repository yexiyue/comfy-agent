//! Convert transport and store failures into safe HTTP responses.
use crate::api::ApiError;
use axum::{
    Json,
    extract::{
        Query,
        rejection::{JsonRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use runtime::store::{StoreError, StoreResult};
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
    (
        status,
        Json(ApiError {
            error: error.to_string(),
        }),
    )
        .into_response()
}
pub(super) fn input<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, Box<Response>> {
    value.map(|Json(v)| v).map_err(|e| {
        (
            if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
                e.status()
            } else {
                StatusCode::BAD_REQUEST
            },
            Json(ApiError {
                error: e.body_text(),
            }),
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

pub(super) fn query<T>(value: Result<Query<T>, QueryRejection>) -> Result<T, Box<Response>> {
    value.map(|Query(value)| value).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(ApiError {
                error: error.body_text(),
            }),
        )
            .into_response()
            .into()
    })
}
