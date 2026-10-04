//! Drive the shared phase engine; persist each phase before the next action.
use super::ExecutionService;
use crate::model::*;
use agent::phase::Action;
use anyhow::Result;
use opentelemetry::trace::TraceContextExt;
use serde_json::{Value, json};
use std::collections::HashMap;
use tracing_opentelemetry::OpenTelemetrySpanExt;

mod model;
mod tool;

impl ExecutionService {
    async fn commit(
        &self,
        run: Run,
        attempt: &Attempt,
        events: Vec<ProgressEvent>,
        tools: Vec<ToolExecution>,
    ) -> Result<Run> {
        let run = self
            .store
            .commit(
                run,
                attempt.clone(),
                events,
                tools,
                self.config.max_event_bytes,
            )
            .await?;
        self.changed.notify_waiters();
        Ok(run)
    }

    pub(super) async fn drive(&self, mut run: Run, run_span: tracing::Span) -> Result<()> {
        let mut attempt = self
            .store
            .attempt(run.attempt_id.as_deref().unwrap())
            .await?;
        let context = tracing::Span::current().context();
        if self.telemetry.enabled && context.span().span_context().is_valid() {
            attempt.trace_id = Some(context.span().span_context().trace_id().to_string());
        }
        if self
            .expected_config
            .as_ref()
            .is_some_and(|(model, hash)| model != &run.model || hash != &run.tool_schema_hash)
        {
            run.status = RunStatus::NeedsAttention;
            run.error = Some("model or tool schema changed; execution requires attention".into());
            attempt.outcome = Some("needs-attention".into());
            self.commit(run, &attempt, vec![], vec![]).await?;
            return Ok(());
        }
        let mut timing = ExecutionTiming::new(run_span);
        let mut step_spans = HashMap::new();
        loop {
            let action = run.checkpoint.next();
            let step = match &action {
                Action::Model { step } => *step,
                _ => run.checkpoint.step,
            };
            let step_span = step_spans
                .entry(step)
                .or_insert_with(|| {
                    let span = tracing::info_span!("step", step);
                    telemetry::attribute(&span, "openinference.span.kind", "CHAIN");
                    telemetry::attribute(&span, "agent.step", step as i64);
                    span
                })
                .clone();
            match action {
                Action::Model { step } => {
                    let Some(next) = self
                        .model_step(run, &mut attempt, step, step_span, &mut timing)
                        .await?
                    else {
                        return Ok(());
                    };
                    run = next;
                }
                Action::Tool(call) => {
                    let Some(next) = self.tool_step(run, &mut attempt, call, step_span).await?
                    else {
                        return Ok(());
                    };
                    run = next;
                }
                Action::StepComplete { .. } => {
                    run.checkpoint.step_completed()?;
                    let events = vec![event(&run, json!({"type":"finish-step"}), false)];
                    run = self.commit(run, &attempt, events, vec![]).await?;
                    step_spans.remove(&step);
                }
                Action::Finished { steps, .. } | Action::StepLimit { steps } => {
                    let outcome = if run.checkpoint.answer.is_some() {
                        "finished"
                    } else {
                        "step-limit"
                    };
                    run.status = if outcome == "finished" {
                        RunStatus::Finished
                    } else {
                        RunStatus::StepLimit
                    };
                    attempt.outcome = Some(outcome.into());
                    let mut metadata = json!({"outcome":outcome,"steps":steps,"runId":run.id,"attemptId":attempt.id,"conversationId":run.conversation_id,"status":run.status,"draft":false});
                    if let Some(trace_id) = &attempt.trace_id {
                        metadata["traceId"] = json!(trace_id);
                    }
                    let events = vec![event(
                        &run,
                        json!({"type":"finish","finishReason":"stop","messageMetadata":metadata}),
                        false,
                    )];
                    self.commit(run, &attempt, events, vec![]).await?;
                    return Ok(());
                }
            }
        }
    }

    async fn text(&self, run: &Run, id: &str, started: &mut bool, text: String) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let mut events = vec![];
        if !*started {
            events.push(event(run, json!({"type":"text-start","id":id}), true));
            *started = true;
        }
        events.push(event(
            run,
            json!({"type":"text-delta","id":id,"delta":text}),
            true,
        ));
        self.store
            .append(&run.id, run.generation, events, self.config.max_event_bytes)
            .await?;
        self.changed.notify_waiters();
        Ok(())
    }
}

fn event(run: &Run, payload: Value, draft: bool) -> ProgressEvent {
    ProgressEvent {
        sequence: 0,
        attempt_id: run.attempt_id.clone().unwrap(),
        step: run.checkpoint.step,
        draft,
        payload,
    }
}

struct ExecutionTiming {
    span: tracing::Span,
    started: std::time::Instant,
    first_text: bool,
}

impl ExecutionTiming {
    fn new(span: tracing::Span) -> Self {
        Self {
            span,
            started: std::time::Instant::now(),
            first_text: false,
        }
    }

    fn record_text(&mut self, text: &str) {
        if !text.is_empty() && !self.first_text {
            self.first_text = true;
            telemetry::attribute(
                &self.span,
                "agent.execution_ttft_ms",
                self.started.elapsed().as_millis() as i64,
            );
        }
    }
}
