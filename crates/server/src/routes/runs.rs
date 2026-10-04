//! Run read models and versioned controls.
use super::response::{error, input};
use crate::AppState;
use crate::api::{ApiError, RunAction, RunDetail, RunStatistics, RunView, UiMessageView};
use crate::views::public_run;
use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    response::{IntoResponse, Response},
};
use runtime::{model::*, store::StoreError};
use serde::Deserialize;
#[utoipa::path(get, path = "/api/runs/{id}", operation_id = "getRun", tag = "runs",
    params(("id" = String, Path)),
    responses((status = 200, body = RunDetail), (status = 404, body = ApiError), (status = 503, body = ApiError)))]
pub async fn run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.service.store.run_with_attempts(&id).await {
        Ok((run, attempts)) => {
            let statistics = RunStatistics {
                model_calls: attempts.iter().map(|attempt| attempt.model_calls).sum(),
                tool_calls: attempts.iter().map(|attempt| attempt.tool_calls).sum(),
                known_tokens: attempts.iter().map(|attempt| attempt.known_tokens).sum(),
                usage_complete: attempts.iter().all(|attempt| attempt.usage_complete),
            };
            Json(RunDetail {
                run: public_run(&run),
                statistics,
                attempts: attempts.into_iter().map(Into::into).collect(),
            })
            .into_response()
        }
        Err(e) => error(e),
    }
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Command {
    expected_version: i64,
    request_id: String,
    #[serde(default)]
    #[schema(value_type = Option<UiMessageView>)]
    message: Option<UiMessage>,
    #[serde(default)]
    expected_revision: Option<i64>,
    conversation_id: String,
}
#[utoipa::path(post, path = "/api/runs/{id}/{action}", operation_id = "controlRun", tag = "runs",
    params(("id" = String, Path), ("action" = RunAction, Path)), request_body = Command,
    responses((status = 200, body = RunView), (status = 400, body = ApiError), (status = 404, body = ApiError),
        (status = 409, body = ApiError), (status = 413, body = ApiError), (status = 503, body = ApiError)))]
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
