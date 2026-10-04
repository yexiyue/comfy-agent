use std::{
    convert::Infallible,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use agent::{AgentEvent, AgentOutcome, run_agent};
use axum::response::sse::Event;
use futures::Stream;
use genai::chat::ChatRequest;
use serde_json::{Value, json};
use tokio::sync::{Notify, mpsc, oneshot};

use crate::AppState;
use opentelemetry::{propagation::TextMapPropagator, trace::TraceContextExt};
use tracing::Instrument;
use tracing::instrument::WithSubscriber;
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub(crate) struct RunContext {
    span: tracing::Span,
    run_id: String,
    trace_id: Option<String>,
    project: String,
    started: std::time::Instant,
}
impl RunContext {
    pub fn new(
        state: &AppState,
        headers: &axum::http::HeaderMap,
        session: Option<String>,
        eval: bool,
    ) -> Self {
        let started = std::time::Instant::now();
        let span = tracing::info_span!(parent: None, "agent.run");
        let carrier: std::collections::HashMap<String, String> = ["traceparent", "tracestate"]
            .into_iter()
            .filter_map(|key| {
                headers
                    .get(key)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| (key.to_owned(), v.to_owned()))
            })
            .collect();
        let context =
            opentelemetry_sdk::propagation::TraceContextPropagator::new().extract(&carrier);
        let _ = span.set_parent(context);
        let run_id = uuid::Uuid::new_v4().to_string();
        telemetry::attribute(&span, "openinference.span.kind", "AGENT");
        telemetry::attribute(&span, "agent.run_id", run_id.clone());
        telemetry::attribute(&span, "agent.steps", 0i64);
        telemetry::attribute(&span, "agent.tool_calls", 0i64);
        if let Some(session) = session {
            telemetry::attribute(&span, "session.id", session);
        }
        let project = if eval {
            &state.telemetry.eval_project
        } else {
            &state.telemetry.project
        }
        .clone();
        telemetry::attribute(&span, "agent.project", project.clone());
        let cx = span.context();
        let sc = cx.span().span_context().clone();
        let trace_id =
            (state.telemetry.enabled && sc.is_valid()).then(|| sc.trace_id().to_string());
        Self {
            span,
            run_id,
            trace_id,
            project,
            started,
        }
    }
}

const QUEUE_CAPACITY: usize = 128;

#[derive(Default)]
struct Encoder {
    step: Option<usize>,
    text: Option<String>,
    block: usize,
}

impl Encoder {
    fn close_text(&mut self, output: &mut Vec<Value>) {
        if let Some(id) = self.text.take() {
            output.push(json!({"type":"text-end", "id":id}));
        }
    }

    fn close_step(&mut self, output: &mut Vec<Value>) {
        self.close_text(output);
        if self.step.take().is_some() {
            output.push(json!({"type":"finish-step"}));
        }
    }

    fn event(&mut self, event: AgentEvent) -> Vec<Value> {
        let mut output = Vec::new();
        match event {
            AgentEvent::StepStarted { step, .. } => {
                self.close_step(&mut output);
                self.step = Some(step);
                output.push(json!({"type":"start-step"}));
            }
            AgentEvent::TextDelta(delta) => {
                let id = self.text.get_or_insert_with(|| {
                    self.block += 1;
                    let id = format!("text-{}", self.block);
                    output.push(json!({"type":"text-start", "id":id}));
                    id
                });
                output.push(json!({"type":"text-delta", "id":id, "delta":delta}));
            }
            AgentEvent::ToolStarted {
                call_id,
                name,
                arguments,
            } => {
                self.close_text(&mut output);
                output.push(json!({"type":"tool-input-available", "toolCallId":call_id, "toolName":name, "input":arguments}));
            }
            AgentEvent::ToolFinished {
                call_id,
                value,
                is_error,
            } => {
                output.push(if is_error {
                    json!({"type":"tool-output-error", "toolCallId":call_id, "errorText":value.get("error").and_then(Value::as_str).unwrap_or("Tool execution failed")})
                } else {
                    json!({"type":"tool-output-available", "toolCallId":call_id, "output":value})
                });
            }
            AgentEvent::StepFinished { .. } => self.close_step(&mut output),
        }
        output
    }
}

enum Terminal {
    Outcome(AgentOutcome),
    Error(&'static str),
    Shutdown,
}

fn json_event(value: Value) -> Event {
    Event::default().data(value.to_string())
}

pub(crate) fn chat_stream(
    state: AppState,
    mut history: ChatRequest,
    context: RunContext,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static {
    let (tx, mut rx) = mpsc::channel(QUEUE_CAPACITY);
    let (done_tx, done_rx) = oneshot::channel();
    let overflow = Arc::new(AtomicBool::new(false));
    let notify = Arc::new(Notify::new());
    let run_id = context.run_id;
    let trace_id = context.trace_id;
    let span = context.span;
    let tasks = state.tasks.clone();
    let policy = Arc::new(state.telemetry.content.clone());
    tasks.spawn(telemetry::PROJECT.scope(context.project,telemetry::CONTENT_POLICY.scope(policy,async move {
        let mut guard = telemetry::RunGuard::new(tracing::Span::current());
        let started = context.started;
        let root = tracing::Span::current();
        let mut first_text = false;
        let event_tx = tx.clone();
        let event_overflow = overflow.clone();
        let event_notify = notify.clone();
        let run = run_agent(
            &state.client,
            &state.model,
            &mut history,
            &state.registry,
            state.max_steps,
            move |event| {
                if !first_text && matches!(&event,AgentEvent::TextDelta(text) if !text.is_empty()) { first_text = true; telemetry::attribute(&root,"agent.request_ttft_ms",started.elapsed().as_secs_f64()*1000.0); }
                if event_tx.try_send(event).is_err() {
                    event_overflow.store(true, Ordering::Relaxed);
                    event_notify.notify_one();
                }
            },
        );
        let terminal = tokio::select! {
            biased;
            _ = tx.closed() => { guard.finish("cancelled"); return; },
            _ = state.shutdown.cancelled() => { guard.finish("shutdown"); Terminal::Shutdown },
            _ = notify.notified() => { guard.finish("queue-overflow"); Terminal::Error("Client is too slow; stream buffer exceeded") },
            result = run => {
                if overflow.load(Ordering::Relaxed) {
                    guard.finish("queue-overflow");
                    Terminal::Error("Client is too slow; stream buffer exceeded")
                } else {
                    match result {
                        Ok(outcome) => { guard.finish(match &outcome { AgentOutcome::Finished{..}=>"finished",AgentOutcome::StepLimit{..}=>"step-limit" }); Terminal::Outcome(outcome) },
                        Err(_error) => {
                            guard.finish("model-error");
                            tracing::error!(category="model-error", "agent request failed");
                            Terminal::Error("Model request failed")
                        }
                    }
                }
            }
        };
        let _ = done_tx.send(terminal);
        // Drop all senders so the response drains queued events before its terminal event.
    }.instrument(span))).with_current_subscriber());
    async_stream::stream! {
        yield Ok(json_event(json!({"type":"start", "messageId":uuid::Uuid::new_v4().to_string()})));
        let mut encoder = Encoder::default();
        while let Some(event) = rx.recv().await {
            for chunk in encoder.event(event) {
                yield Ok(json_event(chunk));
            }
        }
        let mut end = Vec::new();
        encoder.close_step(&mut end);
        for chunk in end { yield Ok(json_event(chunk)); }
        match done_rx.await.unwrap_or(Terminal::Error("Request task stopped unexpectedly")) {
            Terminal::Outcome(outcome) => {
                let (reason, status, steps) = match outcome {
                    AgentOutcome::Finished { steps, .. } => ("stop", "finished", steps),
                    AgentOutcome::StepLimit { steps } => ("other", "step-limit", steps),
                };
                let mut metadata = json!({"outcome":status, "steps":steps,"runId":run_id});
                if let Some(trace_id) = trace_id { metadata["traceId"] = json!(trace_id); }
                yield Ok(json_event(json!({"type":"finish", "finishReason":reason, "messageMetadata":metadata})));
            }
            Terminal::Error(error) => {
                yield Ok(json_event(json!({"type":"error", "errorText":error})));
                yield Ok(json_event(json!({"type":"finish", "finishReason":"error"})));
            }
            Terminal::Shutdown => yield Ok(json_event(json!({"type":"abort", "reason":"Server shutting down"}))),
        }
        yield Ok(Event::default().data("[DONE]"));
    }
}
