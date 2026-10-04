//! Background driver: a persisted phase always precedes the next external action.

use agent::{AgentEvent, ModelResponse, ToolRegistry, phase::Action};
use anyhow::{Result, ensure};
use futures::future::BoxFuture;
use genai::chat::ChatRequest;
use opentelemetry::{propagation::TextMapPropagator, trace::TraceContextExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::instrument::WithSubscriber;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::{
    model::*,
    store::{ConversationStore, StoreResult},
};

pub trait ModelGateway: Send + Sync {
    fn response<'a>(
        &'a self,
        model: &'a str,
        request: ChatRequest,
        on_text: &'a mut (dyn FnMut(String) + Send),
    ) -> BoxFuture<'a, Result<ModelResponse>>;
}

pub struct GenaiGateway(pub genai::Client);
impl ModelGateway for GenaiGateway {
    fn response<'a>(
        &'a self,
        model: &'a str,
        request: ChatRequest,
        on_text: &'a mut (dyn FnMut(String) + Send),
    ) -> BoxFuture<'a, Result<ModelResponse>> {
        Box::pin(async move {
            agent::stream_response(&self.0, model, request, &mut |event| {
                if let AgentEvent::TextDelta(text) = event {
                    on_text(text);
                }
            })
            .await
        })
    }
}

#[derive(Clone)]
pub struct WorkerConfig {
    pub lease_seconds: i64,
    pub heartbeat: Duration,
    pub max_recoveries: usize,
    pub max_event_bytes: i64,
}
impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            lease_seconds: 30,
            heartbeat: Duration::from_secs(5),
            max_recoveries: 5,
            max_event_bytes: 16 * 1024 * 1024,
        }
    }
}

pub struct ExecutionService {
    pub store: Arc<dyn ConversationStore>,
    pub model: Arc<dyn ModelGateway>,
    pub registry: Arc<ToolRegistry>,
    pub config: WorkerConfig,
    pub shutdown: CancellationToken,
    pub changed: Notify,
    pub telemetry: Arc<telemetry::Config>,
    pub expected_config: Option<(String, String)>,
    dispatch: tracing::Dispatch,
    controls: Mutex<HashMap<String, (i64, CancellationToken)>>,
}

impl ExecutionService {
    pub fn new(
        store: Arc<dyn ConversationStore>,
        model: Arc<dyn ModelGateway>,
        registry: Arc<ToolRegistry>,
        config: WorkerConfig,
        shutdown: CancellationToken,
    ) -> Result<Self> {
        ensure!(
            config.lease_seconds > 0
                && !config.heartbeat.is_zero()
                && config.heartbeat < Duration::from_secs(config.lease_seconds as u64),
            "heartbeat must be shorter than lease"
        );
        ensure!(
            config.max_event_bytes > 0,
            "event capacity must be positive"
        );
        Ok(Self {
            store,
            model,
            registry,
            config,
            shutdown,
            telemetry: Arc::new(telemetry::Config::default()),
            expected_config: None,
            dispatch: tracing::dispatcher::get_default(Clone::clone),
            changed: Notify::new(),
            controls: Mutex::new(HashMap::new()),
        })
    }

    pub async fn control(&self, command: Control) -> StoreResult<Run> {
        let id = command.run_id.clone();
        let result = self.store.control(command).await?;
        if let Some((generation, cancel)) = self.controls.lock().unwrap().get(&id)
            && *generation < result.generation
            && matches!(
                result.status,
                RunStatus::Pausing | RunStatus::Cancelled | RunStatus::Superseded
            )
        {
            cancel.cancel();
        }
        self.changed.notify_waiters();
        Ok(result)
    }

    pub fn execute(self: Arc<Self>, task: Dispatch) -> BoxFuture<'static, Result<()>> {
        let dispatch = self.dispatch.clone();
        Box::pin(self.execute_inner(task).with_subscriber(dispatch))
    }

    async fn execute_inner(self: Arc<Self>, task: Dispatch) -> Result<()> {
        if self.shutdown.is_cancelled() {
            return Ok(());
        }
        let Some(run) = self.store.claim(task, self.config.lease_seconds).await? else {
            return Ok(());
        };
        let attempt_id = run
            .attempt_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("claimed run has no attempt"))?;
        let id = run.id.clone();
        let generation = run.generation;
        let cancelled = CancellationToken::new();
        self.controls
            .lock()
            .unwrap()
            .insert(id.clone(), (generation, cancelled.clone()));
        let span = tracing::info_span!(parent:None,"agent.run");
        let carrier: HashMap<String, String> = [
            ("traceparent", run.traceparent.clone()),
            ("tracestate", run.tracestate.clone()),
        ]
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key.into(), value)))
        .collect();
        if self.telemetry.enabled {
            let parent_result = span.set_parent(
                opentelemetry_sdk::propagation::TraceContextPropagator::new().extract(&carrier),
            );
            if let Err(error) = parent_result {
                tracing::warn!(%error,"execution trace parent could not be attached");
            }
        }
        let project = if run.evaluation {
            self.telemetry.eval_project.clone()
        } else {
            self.telemetry.project.clone()
        };
        telemetry::attribute(&span, "openinference.span.kind", "AGENT");
        telemetry::attribute(&span, "agent.project", project.clone());
        telemetry::attribute(&span, "agent.run_id", id.clone());
        if let Some(previous) = &run.supersedes {
            telemetry::attribute(&span, "agent.supersedes", previous.clone());
        }
        telemetry::attribute(&span, "agent.attempt_id", attempt_id.clone());
        telemetry::attribute(&span, "session.id", run.conversation_id.clone());
        let started = std::time::Instant::now();
        let attempt_span = tracing::info_span!(parent:&span,"agent.attempt");
        telemetry::attribute(&attempt_span, "openinference.span.kind", "CHAIN");
        telemetry::attribute(&attempt_span, "agent.project", project.clone());
        telemetry::attribute(&attempt_span, "agent.attempt_id", attempt_id.clone());
        let drive = telemetry::PROJECT
            .scope(
                project,
                telemetry::CONTENT_POLICY.scope(
                    Arc::new(self.telemetry.content.clone()),
                    self.drive(run, span.clone()),
                ),
            )
            .instrument(attempt_span.clone());
        let watchdog = async {
            let mut timer = tokio::time::interval(self.config.heartbeat);
            loop {
                timer.tick().await;
                if !self
                    .store
                    .heartbeat(&id, generation, self.config.lease_seconds)
                    .await?
                {
                    return Ok::<_, anyhow::Error>(());
                }
            }
        };
        let reason = tokio::select! {
            biased;
            _=self.shutdown.cancelled()=>Some("shutdown"),
            _=cancelled.cancelled()=>Some("control"),
            result=watchdog=>{ if let Err(error)=result { tracing::error!(%error,"execution lease monitor failed"); } Some("control") },
            result=drive=>match result { Ok(())=>None,Err(error)=>{ tracing::error!(run_id=%id,%error,"background execution failed");Some("internal-error") } },
        };
        self.controls.lock().unwrap().remove(&id);
        if let Some(reason) = reason {
            self.store
                .settle(&id, &attempt_id, generation, reason)
                .await?;
        }
        if let Ok(attempt) = self.store.attempt(&attempt_id).await {
            telemetry::attribute(
                &span,
                "agent.outcome",
                attempt
                    .outcome
                    .clone()
                    .unwrap_or_else(|| "interrupted".into()),
            );
            telemetry::attribute(
                &attempt_span,
                "agent.outcome",
                attempt
                    .outcome
                    .clone()
                    .unwrap_or_else(|| "interrupted".into()),
            );
            telemetry::attribute(&span, "agent.model_calls", attempt.model_calls as i64);
            telemetry::attribute(&span, "agent.tool_calls", attempt.tool_calls as i64);
            telemetry::attribute(
                &span,
                "agent.tokens.known_total",
                attempt.known_tokens as i64,
            );
            telemetry::attribute(&span, "agent.tokens.complete", attempt.usage_complete);
        }
        if let Ok(run) = self.store.run(&id).await {
            telemetry::attribute(&span, "agent.steps", run.checkpoint.step as i64);
        }
        telemetry::attribute(
            &span,
            "agent.execution_ms",
            started.elapsed().as_millis() as i64,
        );
        self.changed.notify_waiters();
        Ok(())
    }

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

    async fn drive(&self, mut run: Run, run_span: tracing::Span) -> Result<()> {
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
        let started = std::time::Instant::now();
        let mut first_text = false;
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
                    run.checkpoint.begin_model()?;
                    attempt.model_calls += 1;
                    let previous_usage_complete = attempt.usage_complete;
                    attempt.usage_complete = false;
                    run = self
                        .commit(
                            run.clone(),
                            &attempt,
                            vec![event(&run, json!({"type":"start-step"}), true)],
                            vec![],
                        )
                        .await?;
                    let (sender, mut receiver) = mpsc::channel::<String>(128);
                    let overflow = Arc::new(AtomicBool::new(false));
                    let failed = overflow.clone();
                    let mut on_text = move |text| {
                        if sender.try_send(text).is_err() {
                            failed.store(true, Ordering::Relaxed);
                        }
                    };
                    let request = run
                        .checkpoint
                        .history
                        .clone()
                        .with_tools(self.registry.definitions());
                    let model = run.model.clone();
                    let future = self
                        .model
                        .response(&model, request, &mut on_text)
                        .instrument(step_span.clone());
                    tokio::pin!(future);
                    let mut text_started = false;
                    let text_id = format!("{}-step-{step}", run.assistant_id);
                    let response = loop {
                        tokio::select! {
                            biased;
                            Some(text)=receiver.recv()=>{
                                if !text.is_empty() && !first_text {first_text=true;telemetry::attribute(&run_span,"agent.execution_ttft_ms",started.elapsed().as_millis() as i64);}
                                self.text(&run,&text_id,&mut text_started,text).await?;
                            },
                            result=&mut future=>break result,
                        }
                        ensure!(
                            !overflow.load(Ordering::Relaxed),
                            "persistence event queue exceeded"
                        );
                    };
                    while let Ok(text) = receiver.try_recv() {
                        if !text.is_empty() && !first_text {
                            first_text = true;
                            telemetry::attribute(
                                &run_span,
                                "agent.execution_ttft_ms",
                                started.elapsed().as_millis() as i64,
                            );
                        }
                        self.text(&run, &text_id, &mut text_started, text).await?;
                    }
                    ensure!(
                        !overflow.load(Ordering::Relaxed),
                        "persistence event queue exceeded"
                    );
                    let response = match response {
                        Ok(response) => response,
                        Err(error) => {
                            tracing::error!(run_id=%run.id,%error,"model request failed");
                            run.status = RunStatus::Failed;
                            run.error = Some("model request failed".into());
                            attempt.outcome = Some("model-error".into());
                            attempt.usage_complete = false;
                            let mut events = vec![];
                            if text_started {
                                events.push(event(
                                    &run,
                                    json!({"type":"text-end","id":text_id}),
                                    true,
                                ));
                            }
                            events.push(event(&run, json!({"type":"finish-step"}), true));
                            events.push(event(
                                &run,
                                json!({"type":"error","errorText":"Model request failed"}),
                                false,
                            ));
                            self.commit(run, &attempt, events, vec![]).await?;
                            return Ok(());
                        }
                    };
                    if let Some(tokens) = response
                        .usage
                        .as_ref()
                        .and_then(|usage| usage.total_tokens)
                        .and_then(|value| u64::try_from(value).ok())
                    {
                        attempt.known_tokens += tokens;
                        attempt.usage_complete = previous_usage_complete;
                    }
                    run.checkpoint.model_completed(response.content)?;
                    let mut events = vec![];
                    if text_started {
                        events.push(event(&run, json!({"type":"text-end","id":text_id}), false));
                    }
                    if let Some(decision) = &run.checkpoint.decision {
                        for call in decision.tool_calls() {
                            events.push(event(&run,json!({"type":"tool-input-available","toolCallId":call.call_id,"toolName":call.fn_name,"input":call.fn_arguments}),false));
                        }
                    }
                    run = self.commit(run, &attempt, events, vec![]).await?;
                }
                Action::Tool(call) => {
                    let resumed = run.checkpoint.tool_inflight;
                    let policy = self.registry.recovery_policy(&call.fn_name);
                    let previous = self.store.tool(&run.id, &call.call_id).await?;
                    let reconcile = resumed && policy == tools::RecoveryPolicy::Reconcilable;
                    if resumed
                        && (policy == tools::RecoveryPolicy::Conservative
                            || (reconcile
                                && previous
                                    .as_ref()
                                    .and_then(|tool| tool.external_id.as_ref())
                                    .is_none()))
                    {
                        run.status = RunStatus::NeedsAttention;
                        run.error = Some("external operation outcome needs reconciliation".into());
                        attempt.outcome = Some("needs-attention".into());
                        self.commit(run, &attempt, vec![], vec![]).await?;
                        return Ok(());
                    }
                    run.checkpoint.begin_tool()?;
                    attempt.tool_calls += 1;
                    let mut tool = previous.unwrap_or_else(|| ToolExecution {
                        call_id: call.call_id.clone(),
                        step: run.checkpoint.step,
                        name: call.fn_name.clone(),
                        arguments: call.fn_arguments.clone(),
                        recovery: policy.as_str().into(),
                        operation_key: format!("{}/{}", run.id, call.call_id),
                        external_id: None,
                        output: None,
                        is_error: false,
                    });
                    run = self
                        .commit(run, &attempt, vec![], vec![tool.clone()])
                        .await?;
                    let store = self.store.clone();
                    let id = run.id.clone();
                    let call_id = call.call_id.clone();
                    let generation = run.generation;
                    let context = tools::ExecutionContext::new(
                        tool.operation_key.clone(),
                        tool.external_id.clone(),
                        Arc::new(move |external_id| {
                            let store = store.clone();
                            let id = id.clone();
                            let call_id = call_id.clone();
                            Box::pin(async move {
                                store
                                    .record_external(&id, generation, &call_id, external_id)
                                    .await?;
                                Ok(())
                            })
                        }),
                    );
                    let tool_result = {
                        let span =
                            tracing::info_span!(parent:&step_span,"tool",tool_name=%call.fn_name);
                        telemetry::attribute(&span, "openinference.span.kind", "TOOL");
                        telemetry::attribute(&span, "tool.name", call.fn_name.clone());
                        telemetry::content_policy().record(&span, "input", &call.fn_arguments);
                        let result = self
                            .registry
                            .execute_with_context(&call, &context, reconcile)
                            .instrument(span.clone())
                            .await;
                        if let Ok(value) = &result {
                            telemetry::content_policy().record(&span, "output", value);
                        } else {
                            telemetry::error(&span, "tool-error");
                        }
                        result
                    };
                    let (value, is_error) = match tool_result {
                        Ok(value) => (value, false),
                        Err(error) if reconcile => {
                            tracing::error!(run_id=%run.id,%error,"external operation reconciliation failed");
                            run.status = RunStatus::NeedsAttention;
                            run.error =
                                Some("external operation outcome needs reconciliation".into());
                            attempt.outcome = Some("needs-attention".into());
                            self.commit(run, &attempt, vec![], vec![]).await?;
                            return Ok(());
                        }
                        Err(error) => (json!({"error":error.to_string()}), true),
                    };
                    run.checkpoint
                        .tool_completed(&call.call_id, value.clone())?;
                    if let Some(updated) = self.store.tool(&run.id, &call.call_id).await? {
                        tool.external_id = updated.external_id;
                    }
                    tool.output = Some(value.clone());
                    tool.is_error = is_error;
                    let payload = if is_error {
                        json!({"type":"tool-output-error","toolCallId":call.call_id,"errorText":value["error"]})
                    } else {
                        json!({"type":"tool-output-available","toolCallId":call.call_id,"output":value})
                    };
                    let events = vec![event(&run, payload, false)];
                    run = self.commit(run, &attempt, events, vec![tool]).await?;
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
