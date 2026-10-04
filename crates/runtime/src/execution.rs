//! Background driver: a persisted phase always precedes the next external action.

use agent::ToolRegistry;
use anyhow::{Result, ensure};
use futures::future::BoxFuture;
use opentelemetry::propagation::TextMapPropagator;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use tracing::instrument::WithSubscriber;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::{
    model::*,
    store::{ConversationStore, StoreResult},
};

mod driver;
mod gateway;
pub use gateway::{GenaiGateway, ModelDelta, ModelGateway};

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
    pub expected_tool_schema_hash: Option<String>,
    pub allowed_models: Option<Vec<String>>,
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
            expected_tool_schema_hash: None,
            allowed_models: None,
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
}
