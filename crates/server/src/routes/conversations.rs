//! Conversation import, pagination, snapshots and command receipts.
use super::response::{error, input, query, response};
use crate::AppState;
use crate::api::{ApiError, CommandResult, ConversationView, UiMessageView};
use axum::{
    Json,
    extract::{
        Path, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    response::{IntoResponse, Response},
};
use runtime::{model::*, store::StoreError};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Create {
    #[serde(default)]
    #[schema(value_type = Vec<UiMessageView>)]
    messages: Vec<UiMessage>,
}
#[utoipa::path(post, path = "/api/conversations", operation_id = "createConversation", tag = "conversations",
    request_body = Create,
    responses((status = 200, body = ConversationView), (status = 400, body = ApiError),
        (status = 413, body = ApiError), (status = 503, body = ApiError)))]
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
            .await
            .map(ConversationView::from),
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
#[utoipa::path(get, path = "/api/conversations", operation_id = "listConversations", tag = "conversations",
    params(("offset" = Option<i64>, Query, minimum = 0), ("limit" = Option<i64>, Query, minimum = 1, maximum = 100)),
    responses((status = 200, body = Vec<ConversationView>), (status = 400, body = ApiError), (status = 503, body = ApiError)))]
pub async fn list(
    State(state): State<AppState>,
    page: Result<Query<Page>, QueryRejection>,
) -> Response {
    let page = match query(page) {
        Ok(page) => page,
        Err(error) => return *error,
    };
    response(
        state
            .service
            .store
            .list(page.offset, page.limit)
            .await
            .map(|items| {
                items
                    .into_iter()
                    .map(ConversationView::from)
                    .collect::<Vec<_>>()
            }),
    )
}
#[utoipa::path(get, path = "/api/conversations/{id}", operation_id = "getConversation", tag = "conversations",
    params(("id" = String, Path)),
    responses((status = 200, body = ConversationView), (status = 404, body = ApiError), (status = 503, body = ApiError)))]
pub async fn snapshot(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    response(
        state
            .service
            .store
            .conversation(&id)
            .await
            .map(ConversationView::from),
    )
}
#[utoipa::path(get, path = "/api/conversations/{id}/commands/{request_id}", operation_id = "getCommandResult", tag = "conversations",
    params(("id" = String, Path), ("request_id" = String, Path)),
    responses((status = 200, body = CommandResult), (status = 404, body = ApiError), (status = 503, body = ApiError)))]
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
