//! Incremental chat submission and independent stream attachment.
use super::response::{error, input};
use crate::AppState;
use crate::api::{ApiError, UiMessageView};
use crate::stream::sse;
use axum::http::HeaderMap;
use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    response::Response,
};
use runtime::{model::*, store::StoreError};
use serde::Deserialize;

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Chat {
    pub id: String,
    #[schema(value_type = UiMessageView)]
    pub message: UiMessage,
    pub expected_revision: i64,
    pub request_id: String,
    #[serde(default)]
    pub trigger: Option<String>,
}
#[utoipa::path(post, path = "/api/chat", operation_id = "submitChat", tag = "chat",
    request_body = Chat,
    responses((status = 200, description = "UI Message Stream v1 SSE; consume with AI SDK", body = String, content_type = "text/event-stream"),
        (status = 400, body = ApiError), (status = 404, body = ApiError), (status = 409, body = ApiError),
        (status = 413, body = ApiError), (status = 503, body = ApiError)))]
pub async fn chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<Chat>, JsonRejection>,
) -> Response {
    let body = match input(body) {
        Ok(v) => v,
        Err(e) => return *e,
    };
    if state.service.shutdown.is_cancelled() {
        return error(StoreError::Unavailable(anyhow::anyhow!(
            "server shutting down"
        )));
    }
    if body.trigger.as_deref().unwrap_or("submit-message") != "submit-message" {
        return error(StoreError::Invalid("regeneration is not supported".into()));
    }
    let source = headers
        .get("x-agent-run-source")
        .and_then(|v| v.to_str().ok());
    if source.is_some_and(|v| v != "eval") {
        return error(StoreError::Invalid("unsupported run source".into()));
    }
    let command = Submit {
        conversation_id: body.id,
        expected_revision: body.expected_revision,
        request_id: body.request_id,
        message: body.message,
        model: state.model.to_string(),
        max_steps: state.max_steps,
        tool_schema_hash: state.tool_schema_hash.to_string(),
        evaluation: source == Some("eval"),
        traceparent: headers
            .get("traceparent")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        tracestate: headers
            .get("tracestate")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    };
    match state.service.store.submit(command).await {
        Ok(run) => {
            state.service.changed.notify_waiters();
            sse(state, run)
        }
        Err(e) => error(e),
    }
}
