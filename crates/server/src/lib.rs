//! Durable HTTP commands and independent UI Message Stream subscriptions.
pub mod protocol;
mod routes;
mod stream;
mod views;
use agent::{ToolRegistry, agent_tool};
use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    http::{HeaderValue, Method, header},
    routing::{get, post},
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
#[derive(Clone)]
pub struct AppState {
    pub service: Arc<runtime::execution::ExecutionService>,
    pub model: Arc<str>,
    pub max_steps: usize,
    pub tool_schema_hash: Arc<str>,
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
    registry.register(tools::SafeToRetry(AddTool))?;
    Ok(registry)
}

pub fn router(state: AppState, origins: Vec<HeaderValue>) -> Router {
    Router::new()
        .route("/health", get(|| async { Json(json!({"status":"ok"})) }))
        .route("/api/chat", post(routes::chat))
        .route("/api/conversations", get(routes::list).post(routes::create))
        .route("/api/conversations/{id}", get(routes::snapshot))
        .route(
            "/api/conversations/{id}/commands/{request_id}",
            get(routes::receipt),
        )
        .route("/api/runs/{id}", get(routes::run))
        .route("/api/runs/{id}/{action}", post(routes::control))
        .route("/api/chat/{id}/stream", get(stream::stream))
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
