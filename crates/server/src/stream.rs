//! Persistent UI Message Stream replay and subscription lifecycle.
use crate::{AppState, routes::error, views::public_run};
use axum::{
    extract::{Path, State},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
};
use futures::Stream;
use runtime::model::{Run, RunStatus};
use serde_json::json;
use std::{convert::Infallible, sync::Arc, time::Duration};
#[utoipa::path(get, path = "/api/chat/{id}/stream", operation_id = "reconnectChat", tag = "chat",
    params(("id" = String, Path)),
    responses((status = 200, description = "UI Message Stream v1 replay", body = String, content_type = "text/event-stream"),
        (status = 404, body = crate::api::ApiError), (status = 503, body = crate::api::ApiError)))]
pub async fn stream(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.service.store.run(&id).await {
        Ok(run) => sse(state, run),
        Err(e) => error(e),
    }
}
pub(crate) fn sse(state: AppState, run: Run) -> Response {
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

const REPLAY_BATCH_SIZE: i64 = 256;
const PROGRESS_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SLOW_CONSUMER_TIMEOUT: Duration = Duration::from_secs(30);

fn stream_event(payload: serde_json::Value) -> Event {
    Event::default()
        .json_data(payload)
        .expect("JSON values are serializable")
}

fn run_state(run: &Run) -> serde_json::Value {
    json!({"type": "data-run-state", "data": public_run(run), "transient": true})
}

fn subscription(
    service: Arc<runtime::execution::ExecutionService>,
    initial: Run,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static {
    async_stream::stream! {
        let metrics = SubscriptionMetrics::new(&initial, &service.telemetry);
        yield Ok(stream_event(json!({
            "type": "start",
            "messageId": initial.assistant_id,
            "messageMetadata": {"runId": initial.id, "conversationId": initial.conversation_id}
        })));
        let mut after = 0;
        let mut generation = initial.generation;
        let mut boundary = StreamBoundary::default();
        let mut last_state = None;

        'subscription: loop {
            // Register before reading; polling also handles cross-process or lost notifications.
            let changed = service.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let (run, batch) = match service.store.progress(&initial.id, after, REPLAY_BATCH_SIZE).await {
                Ok(progress) => progress,
                Err(error) => {
                    tracing::error!(%error, "stream snapshot failed");
                    yield Ok(stream_event(json!({"type": "error", "errorText": "Storage unavailable"})));
                    break;
                }
            };
            if after == 0 {
                generation = run.generation;
            } else if run.generation != generation {
                // Detach so the client replaces already displayed, invalidated draft blocks.
                for payload in boundary.close_blocks() {
                    yield Ok(stream_event(payload));
                }
                yield Ok(stream_event(run_state(&run)));
                break;
            }

            let full_batch = batch.len() == REPLAY_BATCH_SIZE as usize;
            for event in batch {
                after = event.sequence;
                boundary.observe(&event.payload);
                if event.payload["type"] == "reasoning-delta" { metrics.first_reasoning(); }
                if event.payload["type"] == "text-delta" {
                    metrics.first_text();
                }
                let offered = std::time::Instant::now();
                yield Ok(stream_event(event.payload));
                if offered.elapsed() > SLOW_CONSUMER_TIMEOUT {
                    tracing::info!(run_id = %initial.id, "slow subscription detached");
                    break 'subscription;
                }
            }
            if full_batch {
                continue;
            }
            let state = run_state(&run);
            if last_state.as_ref() != Some(&state) {
                yield Ok(stream_event(state.clone()));
                last_state = Some(state);
            }
            if run.status.is_terminal()
                || matches!(run.status, RunStatus::Paused | RunStatus::NeedsAttention)
                || service.shutdown.is_cancelled()
            {
                if run.status == RunStatus::Failed && !boundary.error_seen {
                    yield Ok(stream_event(json!({
                        "type": "error", "errorText": "Execution failed; see server diagnostics"
                    })));
                }
                break;
            }
            tokio::select! {
                _ = changed => {},
                _ = tokio::time::sleep(PROGRESS_POLL_INTERVAL) => {},
                _ = service.shutdown.cancelled() => {},
            }
        }
        if !boundary.finished {
            for payload in boundary.close_blocks() {
                yield Ok(stream_event(payload));
            }
            yield Ok(stream_event(json!({"type": "abort"})));
        }
        yield Ok(Event::default().data("[DONE]"));
    }
}

/// Keep protocol boundaries coherent on every detach and failure path.
#[derive(Default)]
struct StreamBoundary {
    open_step: bool,
    open_blocks: std::collections::BTreeMap<String, String>,
    finished: bool,
    error_seen: bool,
}

impl StreamBoundary {
    fn observe(&mut self, payload: &serde_json::Value) {
        match payload["type"].as_str().unwrap_or_default() {
            "start-step" => self.open_step = true,
            "finish-step" => self.open_step = false,
            "text-start" | "reasoning-start" => {
                if let Some(id) = payload["id"].as_str() {
                    self.open_blocks.insert(
                        id.to_owned(),
                        if payload["type"] == "reasoning-start" {
                            "reasoning-end"
                        } else {
                            "text-end"
                        }
                        .into(),
                    );
                }
            }
            "text-end" | "reasoning-end" => {
                if let Some(id) = payload["id"].as_str() {
                    self.open_blocks.remove(id);
                }
            }
            "finish" => self.finished = true,
            "error" => self.error_seen = true,
            _ => {}
        }
    }

    fn close_blocks(&mut self) -> Vec<serde_json::Value> {
        let mut events: Vec<_> = std::mem::take(&mut self.open_blocks)
            .into_iter()
            .map(|(id, kind)| json!({"type": kind, "id": id}))
            .collect();
        if std::mem::take(&mut self.open_step) {
            events.push(json!({"type": "finish-step"}));
        }
        events
    }
}

struct SubscriptionMetrics {
    span: tracing::Span,
    started: std::time::Instant,
    first: std::cell::Cell<bool>,
    reasoning: std::cell::Cell<bool>,
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
            reasoning: std::cell::Cell::new(false),
        }
    }
    fn first_reasoning(&self) {
        if !self.reasoning.replace(true) {
            telemetry::attribute(
                &self.span,
                "agent.subscription_first_reasoning_ms",
                self.started.elapsed().as_millis() as i64,
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_closes_reasoning_before_step_without_duplicate_end() {
        let mut boundary = StreamBoundary::default();
        boundary.observe(&json!({"type":"start-step"}));
        boundary.observe(&json!({"type":"text-start","id":"answer"}));
        boundary.observe(&json!({"type":"text-end","id":"answer"}));
        boundary.observe(&json!({"type":"reasoning-start","id":"thinking"}));
        assert_eq!(
            boundary.close_blocks(),
            vec![
                json!({"type":"reasoning-end","id":"thinking"}),
                json!({"type":"finish-step"}),
            ]
        );
        assert!(boundary.close_blocks().is_empty());
    }
}
