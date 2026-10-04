//! Stateless HTTP transport for the agent core.
pub mod protocol;
mod stream;

use std::{sync::Arc, time::Duration};

use agent::{ToolRegistry, agent_tool};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response, Sse, sse::KeepAlive},
    routing::{get, post},
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

#[derive(Clone)]
pub struct AppState {
    pub client: genai::Client,
    pub model: Arc<str>,
    pub max_steps: usize,
    pub registry: Arc<ToolRegistry>,
    pub shutdown: CancellationToken,
    pub telemetry: Arc<telemetry::Config>,
    pub tasks: tokio_util::task::TaskTracker,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AddArgs {
    a: i64,
    b: i64,
}

/// Add two integers, reporting an error if the result overflows.
#[agent_tool]
async fn add(args: AddArgs) -> anyhow::Result<Value> {
    let sum = args
        .a
        .checked_add(args.b)
        .ok_or_else(|| anyhow::anyhow!("integer addition overflow"))?;
    Ok(json!({"sum":sum}))
}

pub fn default_registry() -> anyhow::Result<ToolRegistry> {
    let mut registry = ToolRegistry::default();
    registry.register(AddTool)?;
    Ok(registry)
}

pub fn router(state: AppState, origins: Vec<HeaderValue>) -> Router {
    Router::new()
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/api/chat", post(chat))
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(
            CorsLayer::new()
                .allow_origin(origins)
                .allow_methods([Method::GET, Method::POST])
                .allow_headers([
                    header::CONTENT_TYPE,
                    header::HeaderName::from_static("traceparent"),
                    header::HeaderName::from_static("tracestate"),
                    header::HeaderName::from_static("x-agent-run-source"),
                ])
                .expose_headers([axum::http::HeaderName::from_static(
                    "x-vercel-ai-ui-message-stream",
                )]),
        )
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    input: Result<Json<protocol::ChatInput>, JsonRejection>,
) -> Response {
    let input = match input {
        Ok(Json(input)) => input,
        Err(error) => {
            return (error.status(), Json(json!({"error":error.body_text()}))).into_response();
        }
    };
    let session_id = input.id.clone();
    let history = match input.into_history() {
        Ok(history) => history,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":error.to_string()})),
            )
                .into_response();
        }
    };
    if state.shutdown.is_cancelled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error":"Server shutting down"})),
        )
            .into_response();
    }
    let source = headers
        .get("x-agent-run-source")
        .and_then(|v| v.to_str().ok());
    if source.is_some_and(|v| v != "eval") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"Unsupported run source"})),
        )
            .into_response();
    }
    let context = stream::RunContext::new(&state, &headers, session_id, source == Some("eval"));
    let mut response = Sse::new(stream::chat_stream(state, history, context))
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(10))
                .text("ping"),
        )
        .into_response();
    response.headers_mut().insert(
        "x-vercel-ai-ui-message-stream",
        HeaderValue::from_static("v1"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response
}
