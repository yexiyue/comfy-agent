//! HTTP commands and subscriptions are separate from background execution.
use crate::AppState;
use axum::{
    Json,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
};
use futures::Stream;
use runtime::{
    model::*,
    store::{StoreError, StoreResult},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Arc, time::Duration};
use uuid::Uuid;

pub fn error(error: StoreError) -> Response {
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
fn input<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, Box<Response>> {
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
fn response<T: serde::Serialize>(value: StoreResult<T>) -> Response {
    match value {
        Ok(v) => Json(v).into_response(),
        Err(e) => error(e),
    }
}

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
pub async fn run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.service.store.run_with_attempts(&id).await {
        Ok((run, attempts)) => {
            let mut value = public_run(&run);
            value["statistics"] = json!({"modelCalls":attempts.iter().map(|v|v.model_calls).sum::<usize>(),"toolCalls":attempts.iter().map(|v|v.tool_calls).sum::<usize>(),"knownTokens":attempts.iter().map(|v|v.known_tokens).sum::<u64>(),"usageComplete":attempts.iter().all(|v|v.usage_complete)});
            value["attempts"] = json!(attempts);
            Json(value).into_response()
        }
        Err(e) => error(e),
    }
}
pub fn public_run(run: &Run) -> Value {
    json!({"id":run.id,"conversationId":run.conversation_id,"assistantId":run.assistant_id,"status":run.status,"version":run.version,"generation":run.generation,"attemptId":run.attempt_id,"steps":run.checkpoint.step,"error":run.error,"supersedes":run.supersedes})
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Chat {
    pub id: String,
    pub message: UiMessage,
    pub expected_revision: i64,
    pub request_id: String,
    #[serde(default)]
    pub trigger: Option<String>,
}
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
pub async fn stream(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.service.store.run(&id).await {
        Ok(run) => sse(state, run),
        Err(e) => error(e),
    }
}
fn sse(state: AppState, run: Run) -> Response {
    let mut response = Sse::new(subscription(state.service, run))
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(10))
                .text("ping"),
        )
        .into_response();
    for (name, value) in [
        ("x-vercel-ai-ui-message-stream", "v1"),
        ("cache-control", "no-cache, no-transform"),
        ("x-accel-buffering", "no"),
    ] {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(name),
            axum::http::HeaderValue::from_static(value),
        );
    }
    response
}

fn subscription(
    service: Arc<runtime::execution::ExecutionService>,
    initial: Run,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static {
    async_stream::stream! {
        let metrics=SubscriptionMetrics::new(&initial,&service.telemetry);
        yield Ok(Event::default().json_data(json!({"type":"start","messageId":initial.assistant_id,"messageMetadata":{"runId":initial.id,"conversationId":initial.conversation_id}})).unwrap());
        let mut after=0;let mut generation=initial.generation;let mut open_step=false;let mut texts=std::collections::HashSet::new();let mut finished=false;let mut aborted=false;let mut error_seen=false;let mut last_state=None;
        'subscription:loop {
            // Register before reading. A timer compensates for cross-process and lost notifications.
            let changed=service.changed.notified();tokio::pin!(changed);changed.as_mut().enable();
            let (run,batch)=match service.store.progress(&initial.id,after,256).await { Ok(v)=>v,Err(e)=>{tracing::error!(%e,"stream snapshot failed");yield Ok(Event::default().json_data(json!({"type":"error","errorText":"Storage unavailable"})).unwrap());break;} };
            // A generation change invalidates draft blocks already sent: detach so the client can replace the prefix.
            if after==0 {generation=run.generation;} else if run.generation!=generation {
                for id in texts.drain() {yield Ok(Event::default().json_data(json!({"type":"text-end","id":id})).unwrap());}
                if open_step {yield Ok(Event::default().json_data(json!({"type":"finish-step"})).unwrap());}
                yield Ok(Event::default().json_data(json!({"type":"data-run-state","data":public_run(&run),"transient":true})).unwrap());
                yield Ok(Event::default().json_data(json!({"type":"abort"})).unwrap());aborted=true;open_step=false;break;
            }
            let full=batch.len()==256;
            for event in batch {
                after=event.sequence;
                let p=event.payload;
                match p["type"].as_str().unwrap_or_default() {
                    "start-step"=>open_step=true,
                    "finish-step"=>open_step=false,
                    "text-start"=>{ if let Some(id)=p["id"].as_str() {texts.insert(id.to_owned());} },
                    "text-end"=>{ if let Some(id)=p["id"].as_str() {texts.remove(id);} },
                    "finish"=>finished=true,
                    "error"=>error_seen=true,
                    _=>{},
                }
                if p["type"]=="text-delta" {metrics.first_text();}
                let offered=std::time::Instant::now();
                yield Ok(Event::default().json_data(p).unwrap());
                if offered.elapsed()>Duration::from_secs(30) {tracing::info!(run_id=%initial.id,"slow subscription detached");break 'subscription;}
            }
            if full {continue;}
            let state=public_run(&run);
            if last_state.as_ref()!=Some(&state) {
                yield Ok(Event::default().json_data(json!({"type":"data-run-state","data":state,"transient":true})).unwrap());
                last_state=Some(state);
            }
            if run.status.is_terminal() || matches!(run.status,RunStatus::Paused|RunStatus::NeedsAttention) || service.shutdown.is_cancelled() {
                if run.status==RunStatus::Failed && !error_seen {yield Ok(Event::default().json_data(json!({"type":"error","errorText":"Execution failed; see server diagnostics"})).unwrap());}
                if !finished {
                    for id in texts.drain() {yield Ok(Event::default().json_data(json!({"type":"text-end","id":id})).unwrap());}
                    if open_step {yield Ok(Event::default().json_data(json!({"type":"finish-step"})).unwrap());}
                    yield Ok(Event::default().json_data(json!({"type":"abort"})).unwrap());aborted=true;open_step=false;
                }
                break;
            }
            tokio::select! { _=changed=>{},_=tokio::time::sleep(Duration::from_millis(250))=>{},_=service.shutdown.cancelled()=>{} }
        }
        if !finished && !aborted {
            for id in texts.drain() {yield Ok(Event::default().json_data(json!({"type":"text-end","id":id})).unwrap());}
            if open_step {yield Ok(Event::default().json_data(json!({"type":"finish-step"})).unwrap());}
            yield Ok(Event::default().json_data(json!({"type":"abort"})).unwrap());
        }
        yield Ok(Event::default().data("[DONE]"));
    }
}

struct SubscriptionMetrics {
    span: tracing::Span,
    started: std::time::Instant,
    first: std::cell::Cell<bool>,
}
impl SubscriptionMetrics {
    fn new(run: &Run, config: &telemetry::Config) -> Self {
        let span = tracing::info_span!(parent:None,"agent.subscription");
        telemetry::attribute(&span, "openinference.span.kind", "CHAIN");
        telemetry::attribute(&span, "agent.run_id", run.id.clone());
        telemetry::attribute(
            &span,
            "agent.project",
            if run.evaluation {
                config.eval_project.clone()
            } else {
                config.project.clone()
            },
        );
        Self {
            span,
            started: std::time::Instant::now(),
            first: std::cell::Cell::new(false),
        }
    }
    fn first_text(&self) {
        if !self.first.replace(true) {
            telemetry::attribute(
                &self.span,
                "agent.subscription_ttft_ms",
                self.started.elapsed().as_millis() as i64,
            );
        }
    }
}
impl Drop for SubscriptionMetrics {
    fn drop(&mut self) {
        telemetry::attribute(
            &self.span,
            "agent.subscription_ms",
            self.started.elapsed().as_millis() as i64,
        );
    }
}
