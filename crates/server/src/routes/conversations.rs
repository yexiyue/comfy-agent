//! Conversation import, pagination, snapshots and command receipts.
use super::response::{error, input, response};
use crate::AppState;
use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    response::{IntoResponse, Response},
};
use runtime::{model::*, store::StoreError};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Create {
    #[serde(default)]
    messages: Vec<UiMessage>,
}
pub async fn create(
    State(state): State<AppState>,
    body: Result<Json<Create>, JsonRejection>,
) -> Response {
    let body = match input(body) {
        Ok(v) => v,
        Err(e) => return *e,
    };
    let history = if body.messages.is_empty() {
        genai::chat::ChatRequest::default()
    } else {
        let value = json!({"messages":body.messages});
        let parsed = serde_json::from_value::<crate::protocol::ChatInput>(value)
            .and_then(|v| v.into_import_history().map_err(serde::de::Error::custom));
        match parsed {
            Ok(v) => v,
            Err(e) => return error(StoreError::Invalid(e.to_string())),
        }
    };
    response(
        state
            .service
            .store
            .create(Conversation {
                id: Uuid::new_v4().to_string(),
                revision: 0,
                messages: body.messages,
                history,
                active_run_id: None,
                parent_id: None,
            })
            .await,
    )
}
#[derive(Deserialize)]
pub struct Page {
    #[serde(default)]
    offset: i64,
    #[serde(default = "page_limit")]
    limit: i64,
}
fn page_limit() -> i64 {
    20
}
pub async fn list(State(state): State<AppState>, Query(page): Query<Page>) -> Response {
    response(state.service.store.list(page.offset, page.limit).await)
}
pub async fn snapshot(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    response(state.service.store.conversation(&id).await)
}
pub async fn receipt(
    State(state): State<AppState>,
    Path((id, request_id)): Path<(String, String)>,
) -> Response {
    if let Err(e) = state.service.store.conversation(&id).await {
        return error(e);
    }
    match state.service.store.command_result(&id, &request_id).await {
        Ok(Some(value)) => Json(value).into_response(),
        Ok(None) => error(StoreError::NotFound),
        Err(e) => error(e),
    }
}
