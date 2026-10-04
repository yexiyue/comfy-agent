//! Run read models and versioned controls.
use super::response::{error, input};
use crate::AppState;
use crate::views::public_run;
use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    response::{IntoResponse, Response},
};
use runtime::{model::*, store::StoreError};
use serde::Deserialize;
use serde_json::json;
pub async fn run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.service.store.run_with_attempts(&id).await {
        Ok((run, attempts)) => {
            let mut value = public_run(&run);
            value["statistics"] = json!({
                "modelCalls": attempts.iter().map(|attempt| attempt.model_calls).sum::<usize>(),
                "toolCalls": attempts.iter().map(|attempt| attempt.tool_calls).sum::<usize>(),
                "knownTokens": attempts.iter().map(|attempt| attempt.known_tokens).sum::<u64>(),
                "usageComplete": attempts.iter().all(|attempt| attempt.usage_complete),
            });
            value["attempts"] = json!(attempts);
            Json(value).into_response()
        }
        Err(e) => error(e),
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Command {
    expected_version: i64,
    request_id: String,
    #[serde(default)]
    message: Option<UiMessage>,
    #[serde(default)]
    expected_revision: Option<i64>,
    conversation_id: String,
}
pub async fn control(
    State(state): State<AppState>,
    Path((id, action)): Path<(String, String)>,
    body: Result<Json<Command>, JsonRejection>,
) -> Response {
    let body = match input(body) {
        Ok(v) => v,
        Err(e) => return *e,
    };
    match state.service.store.run(&id).await {
        Ok(run) if run.conversation_id == body.conversation_id => {}
        Ok(_) => return error(StoreError::NotFound),
        Err(e) => return error(e),
    }
    let action = match action.as_str() {
        "pause" => ControlAction::Pause,
        "resume" => ControlAction::Resume,
        "cancel" => ControlAction::Cancel,
        "steer" => match (body.message, body.expected_revision) {
            (Some(message), Some(expected_revision)) => ControlAction::Steer {
                message,
                expected_revision,
            },
            _ => {
                return error(StoreError::Invalid(
                    "steer requires message and expectedRevision".into(),
                ));
            }
        },
        _ => return error(StoreError::NotFound),
    };
    match state
        .service
        .control(Control {
            run_id: id,
            expected_version: body.expected_version,
            request_id: body.request_id,
            action,
        })
        .await
    {
        Ok(run) => Json(public_run(&run)).into_response(),
        Err(e) => error(e),
    }
}
