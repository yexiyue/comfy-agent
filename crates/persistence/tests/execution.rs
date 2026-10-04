use agent::{ModelResponse, ToolRegistry};
use anyhow::Result;
use futures::future::BoxFuture;
use genai::chat::{ChatRequest, MessageContent, ToolCall};
use persistence::{migration, queue::QueueRuntime, repository::PostgresStore};
use runtime::{
    execution::{ExecutionService, ModelGateway, WorkerConfig},
    model::*,
    store::ConversationStore,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tools::{AgentTool, SafeToRetry};
use uuid::Uuid;

struct MockModel(AtomicUsize);
impl ModelGateway for MockModel {
    fn response<'a>(
        &'a self,
        _model: &'a str,
        request: ChatRequest,
        on_text: &'a mut (dyn FnMut(String) + Send),
    ) -> BoxFuture<'a, Result<ModelResponse>> {
        Box::pin(async move {
            let call = self.0.fetch_add(1, Ordering::SeqCst);
            let content = if call == 0 {
                MessageContent::from_tool_calls(
                    ["first", "second"]
                        .into_iter()
                        .map(|name| ToolCall {
                            call_id: format!("call-{name}"),
                            fn_name: name.into(),
                            fn_arguments: json!({}),
                            thought_signatures: Some(vec!["retained".into()]),
                        })
                        .collect(),
                )
            } else {
                let encoded = serde_json::to_value(request)?.to_string();
                assert!(
                    encoded.contains("call-first")
                        && encoded.contains("call-second")
                        && encoded.contains("retained")
                );
                on_text("completed".into());
                MessageContent::from_text("completed")
            };
            Ok(ModelResponse {
                content,
                usage: None,
                stop_reason: None,
            })
        })
    }
}
struct ProbeTool {
    name: &'static str,
    calls: Arc<AtomicUsize>,
    started: Arc<Notify>,
    block_once: bool,
}
impl AgentTool for ProbeTool {
    fn name(&self) -> &'static str {
        self.name
    }
    fn description(&self) -> &'static str {
        "local deterministic tool"
    }
    fn schema(&self) -> Value {
        json!({"type":"object","properties":{}})
    }
    fn execute(&self, _arguments: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            if self.block_once && call == 0 {
                std::future::pending::<()>().await;
            }
            Ok(json!({"name":self.name,"completed":true}))
        })
    }
}

async fn setup(
    block: bool,
) -> Result<(
    Arc<ExecutionService>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<Notify>,
    Run,
    String,
)> {
    let parent = std::env::var("TEST_DATABASE_URL")?;
    anyhow::ensure!(!parent.contains('?') && parent.ends_with("_test"));
    let name = format!("agent_exec_{}_test", Uuid::new_v4().simple());
    let pool = apalis_postgres::PgPool::connect(&parent).await?;
    sqlx::QueryBuilder::<sqlx::Postgres>::new("CREATE DATABASE ")
        .push(&name)
        .build()
        .execute(&pool)
        .await?;
    pool.close().await;
    let prefix = parent.rsplit_once('/').unwrap().0;
    let url = format!("{prefix}/{name}");
    let mut db = persistence::connect(&url).await?;
    migration::migrate(&mut db).await?;
    let store = Arc::new(PostgresStore::open(&url).await?);
    let conversation = store
        .create(Conversation {
            id: Uuid::new_v4().to_string(),
            revision: 0,
            messages: vec![],
            history: ChatRequest::default(),
            active_run_id: None,
            parent_id: None,
        })
        .await?;
    let first = Arc::new(AtomicUsize::new(0));
    let second = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let mut registry = ToolRegistry::default();
    registry.register(SafeToRetry(ProbeTool {
        name: "first",
        calls: first.clone(),
        started: Arc::new(Notify::new()),
        block_once: false,
    }))?;
    registry.register(SafeToRetry(ProbeTool {
        name: "second",
        calls: second.clone(),
        started: started.clone(),
        block_once: block,
    }))?;
    let run = store
        .submit(Submit {
            conversation_id: conversation.id,
            expected_revision: 0,
            request_id: Uuid::new_v4().to_string(),
            message: UiMessage {
                id: Uuid::new_v4().to_string(),
                role: "user".into(),
                parts: vec![json!({"type":"text","text":"execute two tools"})],
                metadata: None,
            },
            model: "mock".into(),
            max_steps: 2,
            tool_schema_hash: "mock".into(),
            evaluation: false,
            traceparent: None,
            tracestate: None,
        })
        .await?;
    let config = WorkerConfig {
        heartbeat: Duration::from_millis(50),
        ..Default::default()
    };
    let service = Arc::new(ExecutionService::new(
        store,
        Arc::new(MockModel(AtomicUsize::new(0))),
        Arc::new(registry),
        config,
        CancellationToken::new(),
    )?);
    Ok((service, first, second, started, run, url))
}

async fn cleanup(url: &str) -> Result<()> {
    let name = url.rsplit_once('/').unwrap().1;
    anyhow::ensure!(
        name.starts_with("agent_exec_")
            && name.ends_with("_test")
            && name
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
    );
    let pool = apalis_postgres::PgPool::connect(&std::env::var("TEST_DATABASE_URL")?).await?;
    sqlx::QueryBuilder::<sqlx::Postgres>::new("DROP DATABASE ")
        .push(name)
        .push(" WITH (FORCE)")
        .build()
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn lost_control_notification_is_stopped_by_the_lease_watchdog() -> Result<()> {
    let (service, first, second, started, run, url) = setup(true).await?;
    let execution = tokio::spawn(service.clone().execute(Dispatch {
        run_id: run.id.clone(),
        dispatch: run.dispatch,
    }));
    tokio::time::timeout(Duration::from_secs(5), started.notified()).await?;
    let current = service.store.run(&run.id).await?;
    // Bypass the local notifier, as when a different server accepts the command.
    service
        .store
        .control(Control {
            run_id: run.id.clone(),
            expected_version: current.version,
            request_id: Uuid::new_v4().to_string(),
            action: ControlAction::Pause,
        })
        .await?;
    tokio::time::timeout(Duration::from_secs(5), execution).await???;
    assert_eq!(service.store.run(&run.id).await?.status, RunStatus::Paused);
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(second.load(Ordering::SeqCst), 1);
    cleanup(&url).await
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn recovery_limit_requires_attention_without_repeating_tools() -> Result<()> {
    let (service, first, second, started, run, url) = setup(true).await?;
    let execution = tokio::spawn(service.clone().execute(Dispatch {
        run_id: run.id.clone(),
        dispatch: run.dispatch,
    }));
    tokio::time::timeout(Duration::from_secs(5), started.notified()).await?;
    execution.abort();
    let _ = execution.await;
    let mut db = persistence::connect(&url).await?;
    toasty::sql::statement(
        "UPDATE agent_runs SET lease_until=NOW()-INTERVAL '1 second' WHERE id=$1",
    )
    .bind(&run.id)
    .exec(&mut db)
    .await?;
    assert_eq!(service.store.recover(0).await?, 1);
    let recovered = service.store.run(&run.id).await?;
    assert_eq!(recovered.status, RunStatus::NeedsAttention);
    service
        .clone()
        .execute(Dispatch {
            run_id: run.id,
            dispatch: recovered.dispatch,
        })
        .await?;
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(second.load(Ordering::SeqCst), 1);
    cleanup(&url).await
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn progress_capacity_failure_stops_before_an_uncheckpointed_model_call() -> Result<()> {
    let (service, first, second, _, run, url) = setup(false).await?;
    let model = Arc::new(MockModel(AtomicUsize::new(0)));
    let bounded = Arc::new(ExecutionService::new(
        service.store.clone(),
        model.clone(),
        service.registry.clone(),
        WorkerConfig {
            max_event_bytes: 1,
            ..service.config.clone()
        },
        CancellationToken::new(),
    )?);
    bounded
        .execute(Dispatch {
            run_id: run.id.clone(),
            dispatch: run.dispatch,
        })
        .await?;
    assert_eq!(service.store.run(&run.id).await?.status, RunStatus::Failed);
    assert_eq!(model.0.load(Ordering::SeqCst), 0);
    assert_eq!(first.load(Ordering::SeqCst), 0);
    assert_eq!(second.load(Ordering::SeqCst), 0);
    assert!(service.store.events(&run.id, 0, 100).await?.is_empty());
    cleanup(&url).await
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn tool_pause_and_resume_reuses_decision_and_only_repeats_incomplete_action() -> Result<()> {
    let (service, first, second, started, run, url) = setup(true).await?;
    let execution = tokio::spawn(service.clone().execute(Dispatch {
        run_id: run.id.clone(),
        dispatch: run.dispatch,
    }));
    tokio::time::timeout(Duration::from_secs(5), started.notified()).await?;
    let current = service.store.run(&run.id).await?;
    service
        .control(Control {
            run_id: run.id.clone(),
            expected_version: current.version,
            request_id: Uuid::new_v4().to_string(),
            action: ControlAction::Pause,
        })
        .await?;
    tokio::time::timeout(Duration::from_secs(5), execution).await???;
    let paused = service.store.run(&run.id).await?;
    assert_eq!(paused.status, RunStatus::Paused);
    assert_eq!(paused.checkpoint.next_tool, 1);
    let resumed = service
        .control(Control {
            run_id: run.id.clone(),
            expected_version: paused.version,
            request_id: Uuid::new_v4().to_string(),
            action: ControlAction::Resume,
        })
        .await?;
    service
        .clone()
        .execute(Dispatch {
            run_id: resumed.id.clone(),
            dispatch: resumed.dispatch,
        })
        .await?;
    let finished = service.store.run(&run.id).await?;
    assert_eq!(finished.status, RunStatus::Finished);
    assert_eq!(finished.checkpoint.step, 2);
    assert_ne!(finished.attempt_id, paused.attempt_id);
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(second.load(Ordering::SeqCst), 2);
    assert_eq!(finished.checkpoint.history.messages.len(), 5);
    cleanup(&url).await
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn apalis_outbox_runs_without_an_http_subscriber() -> Result<()> {
    let (service, first, second, _, run, url) = setup(false).await?;
    let pool = apalis_postgres::PgPool::connect(&url).await?;
    apalis_postgres::PostgresStorage::setup(&pool).await?;
    // Simulate publication succeeding and the publisher crashing before marking the outbox.
    use apalis::prelude::*;
    let mut published = apalis_postgres::PostgresStorage::<Dispatch>::new(&pool)
        .with_config(apalis_postgres::Config::default().queue("comfy-agent-runs"));
    published
        .push(Dispatch {
            run_id: run.id.clone(),
            dispatch: run.dispatch,
        })
        .await?;
    assert!(!service.store.outbox(64).await?.is_empty());
    let queue = QueueRuntime::open(&url, &service, 2).await?;
    let task = tokio::spawn(queue.run(service.clone()));
    let second_worker = tokio::spawn(
        QueueRuntime::open(&url, &service, 2)
            .await?
            .run(service.clone()),
    );
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let current = service.store.run(&run.id).await?;
            if current.status.is_terminal() {
                return Ok::<_, anyhow::Error>(current);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    service.shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(10), task).await???;
    tokio::time::timeout(Duration::from_secs(10), second_worker).await???;
    let completed = result??;
    assert_eq!(completed.status, RunStatus::Finished);
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(second.load(Ordering::SeqCst), 1);
    let events = service.store.events(&run.id, 0, 100).await?;
    assert!(events.iter().any(|event| event.payload["type"] == "finish"));
    cleanup(&url).await
}

struct InterruptModel {
    calls: Arc<AtomicUsize>,
    started: Arc<Notify>,
    interrupts: usize,
}
impl ModelGateway for InterruptModel {
    fn response<'a>(
        &'a self,
        _model: &'a str,
        _request: ChatRequest,
        text: &'a mut (dyn FnMut(String) + Send),
    ) -> BoxFuture<'a, Result<ModelResponse>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call < self.interrupts {
                text(format!("abandoned-{call}"));
                self.started.notify_one();
                std::future::pending::<()>().await;
            }
            let content = MessageContent::from_parts(vec![
                genai::chat::ContentPart::Text("fresh".into()),
                genai::chat::ContentPart::ToolCall(ToolCall {
                    call_id: format!("call-{call}"),
                    fn_name: "first".into(),
                    fn_arguments: json!({}),
                    thought_signatures: None,
                }),
            ]);
            text("fresh".into());
            Ok(ModelResponse {
                content,
                usage: None,
                stop_reason: None,
            })
        })
    }
}
#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn repeated_model_pause_drops_drafts_preserves_budget_and_counts_real_calls() -> Result<()> {
    let (service, first, _, _, mut run, url) = setup(false).await?;
    // The accepted run's two-step budget stays unchanged across every attempt.
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(Notify::new());
    let mut inner = Arc::try_unwrap(service).ok().unwrap();
    inner.model = Arc::new(InterruptModel {
        calls: calls.clone(),
        started: started.clone(),
        interrupts: 2,
    });
    let service = Arc::new(inner);
    for _ in 0..2 {
        let execution = tokio::spawn(service.clone().execute(Dispatch {
            run_id: run.id.clone(),
            dispatch: run.dispatch,
        }));
        tokio::time::timeout(Duration::from_secs(3), started.notified()).await?;
        let current = service.store.run(&run.id).await?;
        service
            .control(Control {
                run_id: run.id.clone(),
                expected_version: current.version,
                request_id: Uuid::new_v4().to_string(),
                action: ControlAction::Pause,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        tokio::time::timeout(Duration::from_secs(3), execution).await???;
        let paused = service.store.run(&run.id).await?;
        assert_eq!(paused.status, RunStatus::Paused);
        assert_eq!(paused.checkpoint.step, 1);
        assert_eq!(paused.checkpoint.history.messages.len(), 1);
        run = service
            .control(Control {
                run_id: run.id.clone(),
                expected_version: paused.version,
                request_id: Uuid::new_v4().to_string(),
                action: ControlAction::Resume,
            })
            .await?;
    }
    service
        .clone()
        .execute(Dispatch {
            run_id: run.id.clone(),
            dispatch: run.dispatch,
        })
        .await?;
    let finished = service.store.run(&run.id).await?;
    assert_eq!(finished.status, RunStatus::StepLimit);
    assert_eq!(finished.checkpoint.step, 2);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(first.load(Ordering::SeqCst), 2);
    let attempts = service.store.attempts(&run.id).await?;
    assert_eq!(attempts.len(), 3);
    assert_eq!(attempts.iter().map(|v| v.model_calls).sum::<usize>(), 4);
    assert!(attempts.iter().all(|v| !v.usage_complete));
    let events = service.store.events(&run.id, 0, 100).await?;
    assert!(
        events
            .iter()
            .all(|v| !v.payload.to_string().contains("abandoned"))
    );
    assert!(events.iter().all(|v| !v.draft));
    let snapshot = service.store.conversation(&run.conversation_id).await?;
    assert!(!serde_json::to_string(&snapshot.messages)?.contains("abandoned"));
    drop(service);
    cleanup(&url).await
}

struct ExternalTool {
    policy: tools::RecoveryPolicy,
    calls: Arc<AtomicUsize>,
    queries: Arc<AtomicUsize>,
    started: Arc<Notify>,
}
impl AgentTool for ExternalTool {
    fn name(&self) -> &'static str {
        "second"
    }
    fn description(&self) -> &'static str {
        "simulated external submission"
    }
    fn schema(&self) -> Value {
        json!({"type":"object"})
    }
    fn recovery_policy(&self) -> tools::RecoveryPolicy {
        self.policy
    }
    fn execute(&self, _: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async { anyhow::bail!("execution context required") })
    }
    fn execute_with_context<'a>(
        &'a self,
        _: Value,
        context: &'a tools::ExecutionContext,
    ) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.policy == tools::RecoveryPolicy::Reconcilable {
                context.record_external_id("external-job-1".into()).await?;
            }
            self.started.notify_one();
            std::future::pending::<Result<Value>>().await
        })
    }
    fn reconcile<'a>(
        &'a self,
        _: Value,
        context: &'a tools::ExecutionContext,
    ) -> BoxFuture<'a, Result<Value>> {
        Box::pin(async move {
            assert_eq!(context.external_id.as_deref(), Some("external-job-1"));
            self.queries.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"originalJob":true}))
        })
    }
}
#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn conservative_side_effects_need_attention_and_external_ids_reconcile_without_resubmit()
-> Result<()> {
    for policy in [
        tools::RecoveryPolicy::Conservative,
        tools::RecoveryPolicy::Reconcilable,
    ] {
        let (service, first, _, _, run, url) = setup(false).await?;
        let calls = Arc::new(AtomicUsize::new(0));
        let queries = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Notify::new());
        let mut registry = ToolRegistry::default();
        registry.register(SafeToRetry(ProbeTool {
            name: "first",
            calls: first.clone(),
            started: Arc::new(Notify::new()),
            block_once: false,
        }))?;
        registry.register(ExternalTool {
            policy,
            calls: calls.clone(),
            queries: queries.clone(),
            started: started.clone(),
        })?;
        let mut inner = Arc::try_unwrap(service).ok().unwrap();
        inner.registry = Arc::new(registry);
        let service = Arc::new(inner);
        let execution = tokio::spawn(service.clone().execute(Dispatch {
            run_id: run.id.clone(),
            dispatch: run.dispatch,
        }));
        tokio::time::timeout(Duration::from_secs(3), started.notified()).await?;
        let current = service.store.run(&run.id).await?;
        service
            .control(Control {
                run_id: run.id.clone(),
                expected_version: current.version,
                request_id: Uuid::new_v4().to_string(),
                action: ControlAction::Pause,
            })
            .await
            .map_err(|e| anyhow::anyhow!("{e:?}"))?;
        tokio::time::timeout(Duration::from_secs(3), execution).await???;
        let paused = service.store.run(&run.id).await?;
        if policy == tools::RecoveryPolicy::Conservative {
            assert_eq!(paused.status, RunStatus::NeedsAttention);
            assert!(
                service
                    .control(Control {
                        run_id: run.id.clone(),
                        expected_version: paused.version,
                        request_id: Uuid::new_v4().to_string(),
                        action: ControlAction::Resume
                    })
                    .await
                    .is_err()
            );
        } else {
            assert_eq!(paused.status, RunStatus::Paused);
            let tool = service.store.tool(&run.id, "call-second").await?.unwrap();
            assert_eq!(tool.external_id.as_deref(), Some("external-job-1"));
            let before = service.store.conversation(&run.conversation_id).await?;
            let steering = service
                .control(Control {
                    run_id: run.id.clone(),
                    expected_version: paused.version,
                    request_id: Uuid::new_v4().to_string(),
                    action: ControlAction::Steer {
                        expected_revision: before.revision,
                        message: UiMessage {
                            id: Uuid::new_v4().to_string(),
                            role: "user".into(),
                            parts: vec![json!({"type":"text","text":"replace pending work"})],
                            metadata: None,
                        },
                    },
                })
                .await;
            assert!(matches!(
                steering,
                Err(runtime::store::StoreError::Conflict)
            ));
            assert_eq!(service.store.run(&run.id).await?.status, RunStatus::Paused);
            assert_eq!(
                service
                    .store
                    .conversation(&run.conversation_id)
                    .await?
                    .revision,
                before.revision
            );
            assert_eq!(queries.load(Ordering::SeqCst), 0);
            let resumed = service
                .control(Control {
                    run_id: run.id.clone(),
                    expected_version: paused.version,
                    request_id: Uuid::new_v4().to_string(),
                    action: ControlAction::Resume,
                })
                .await?;
            service
                .clone()
                .execute(Dispatch {
                    run_id: run.id.clone(),
                    dispatch: resumed.dispatch,
                })
                .await?;
            assert_eq!(
                service.store.run(&run.id).await?.status,
                RunStatus::Finished
            );
            assert_eq!(queries.load(Ordering::SeqCst), 1);
            assert_eq!(
                service
                    .store
                    .tool(&run.id, "call-second")
                    .await?
                    .unwrap()
                    .operation_key,
                tool.operation_key
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(first.load(Ordering::SeqCst), 1);
        drop(service);
        cleanup(&url).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn tool_errors_are_committed_and_do_not_stop_the_following_model_step() -> Result<()> {
    let (service, _, second, _, run, url) = setup(false).await?;
    let mut registry = ToolRegistry::default();
    registry.register(SafeToRetry(ProbeTool {
        name: "second",
        calls: second.clone(),
        started: Arc::new(Notify::new()),
        block_once: false,
    }))?;
    let mut inner = Arc::try_unwrap(service).ok().unwrap();
    inner.registry = Arc::new(registry);
    let service = Arc::new(inner);
    service
        .clone()
        .execute(Dispatch {
            run_id: run.id.clone(),
            dispatch: run.dispatch,
        })
        .await?;
    let finished = service.store.run(&run.id).await?;
    assert_eq!(finished.status, RunStatus::Finished);
    assert_eq!(finished.checkpoint.step, 2);
    assert!(
        service
            .store
            .tool(&run.id, "call-first")
            .await?
            .unwrap()
            .is_error
    );
    assert!(
        service
            .store
            .events(&run.id, 0, 100)
            .await?
            .iter()
            .any(|e| e.payload["type"] == "tool-output-error")
    );
    assert!(serde_json::to_string(&finished.checkpoint.history)?.contains("error"));
    assert_eq!(second.load(Ordering::SeqCst), 1);
    drop(service);
    cleanup(&url).await
}
#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn lost_worker_lease_recovers_remaining_tool_and_pausing_never_auto_resumes() -> Result<()> {
    for pausing in [false, true] {
        let (service, first, second, started, run, url) = setup(true).await?;
        let execution = tokio::spawn(service.clone().execute(Dispatch {
            run_id: run.id.clone(),
            dispatch: run.dispatch,
        }));
        tokio::time::timeout(Duration::from_secs(3), started.notified()).await?;
        let original = service.store.run(&run.id).await?;
        if pausing {
            service
                .store
                .control(Control {
                    run_id: run.id.clone(),
                    expected_version: original.version,
                    request_id: Uuid::new_v4().to_string(),
                    action: ControlAction::Pause,
                })
                .await?;
        }
        // Abrupt task loss intentionally skips cooperative settlement, like a process kill.
        execution.abort();
        let _ = execution.await;
        let mut db = persistence::connect(&url).await?;
        toasty::sql::statement(
            "UPDATE agent_runs SET lease_until=NOW()-INTERVAL '1 second' WHERE id=$1",
        )
        .bind(&run.id)
        .exec(&mut db)
        .await?;
        assert_eq!(service.store.recover(5).await?, 1);
        let recovered = service.store.run(&run.id).await?;
        assert!(
            service
                .store
                .append(&run.id, original.generation, vec![], 1024)
                .await
                .is_err()
        );
        if pausing {
            assert_eq!(recovered.status, RunStatus::Paused);
            assert_eq!(second.load(Ordering::SeqCst), 1);
        } else {
            assert_eq!(recovered.status, RunStatus::Queued);
            service
                .clone()
                .execute(Dispatch {
                    run_id: run.id.clone(),
                    dispatch: recovered.dispatch,
                })
                .await?;
            assert_eq!(
                service.store.run(&run.id).await?.status,
                RunStatus::Finished
            );
            assert_eq!(second.load(Ordering::SeqCst), 2);
        }
        assert_eq!(first.load(Ordering::SeqCst), 1);
        drop(db);
        drop(service);
        cleanup(&url).await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn steer_reuses_completed_results_closes_pending_calls_and_discards_old_todo() -> Result<()> {
    let (service, first, second, started, run, url) = setup(true).await?;
    let execution = tokio::spawn(service.clone().execute(Dispatch {
        run_id: run.id.clone(),
        dispatch: run.dispatch,
    }));
    tokio::time::timeout(Duration::from_secs(3), started.notified()).await?;
    let current = service.store.run(&run.id).await?;
    service
        .control(Control {
            run_id: run.id.clone(),
            expected_version: current.version,
            request_id: Uuid::new_v4().to_string(),
            action: ControlAction::Pause,
        })
        .await?;
    tokio::time::timeout(Duration::from_secs(3), execution).await???;
    let paused = service.store.run(&run.id).await?;
    let conversation = service.store.conversation(&run.conversation_id).await?;
    let replacement = service
        .control(Control {
            run_id: run.id.clone(),
            expected_version: paused.version,
            request_id: Uuid::new_v4().to_string(),
            action: ControlAction::Steer {
                message: UiMessage {
                    id: Uuid::new_v4().to_string(),
                    role: "user".into(),
                    parts: vec![json!({"type":"text","text":"use another plan"})],
                    metadata: None,
                },
                expected_revision: conversation.revision,
            },
        })
        .await?;
    assert_eq!(
        service.store.run(&run.id).await?.status,
        RunStatus::Superseded
    );
    assert_eq!(replacement.supersedes.as_deref(), Some(run.id.as_str()));
    let context = serde_json::to_string(&replacement.checkpoint.history)?;
    assert!(
        context.contains("call-first")
            && context.contains("call-second")
            && context.contains("retained")
            && context.contains("unknown")
    );
    service
        .clone()
        .execute(Dispatch {
            run_id: replacement.id.clone(),
            dispatch: replacement.dispatch,
        })
        .await?;
    assert_eq!(
        service.store.run(&replacement.id).await?.status,
        RunStatus::Finished
    );
    assert_eq!(first.load(Ordering::SeqCst), 1);
    assert_eq!(second.load(Ordering::SeqCst), 1);
    let messages = service
        .store
        .conversation(&run.conversation_id)
        .await?
        .messages;
    assert_eq!(messages.len(), 4);
    assert_eq!(messages[1].id, run.assistant_id);
    assert_eq!(messages[3].id, replacement.assistant_id);
    assert!(
        service
            .store
            .append(&run.id, paused.generation, vec![], 1024)
            .await
            .is_err()
    );
    drop(service);
    cleanup(&url).await
}

struct LoggedTool {
    name: &'static str,
    marker: std::path::PathBuf,
}
impl AgentTool for LoggedTool {
    fn name(&self) -> &'static str {
        self.name
    }
    fn description(&self) -> &'static str {
        "process fixture"
    }
    fn schema(&self) -> Value {
        json!({"type":"object"})
    }
    fn execute(&self, _: Value) -> BoxFuture<'_, Result<Value>> {
        Box::pin(async move {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.marker)?;
            writeln!(file, "{}", self.name)?;
            file.sync_all()?;
            if self.name == "second" {
                std::future::pending::<()>().await;
            }
            Ok(json!({"name":self.name,"completed":true}))
        })
    }
}
#[tokio::test]
#[ignore = "internal child process fixture"]
async fn fixture_process() -> Result<()> {
    let Ok(url) = std::env::var("AGENT_FIXTURE_DATABASE") else {
        return Ok(());
    };
    anyhow::ensure!(url.ends_with("_test"));
    let run_id = std::env::var("AGENT_FIXTURE_RUN")?;
    let marker = std::path::PathBuf::from(std::env::var("AGENT_FIXTURE_MARKER")?);
    let store = Arc::new(PostgresStore::open(&url).await?);
    let run = store.run(&run_id).await?;
    let mut registry = ToolRegistry::default();
    for name in ["first", "second"] {
        registry.register(SafeToRetry(LoggedTool {
            name,
            marker: marker.clone(),
        }))?;
    }
    let service = Arc::new(ExecutionService::new(
        store,
        Arc::new(MockModel(AtomicUsize::new(0))),
        Arc::new(registry),
        WorkerConfig {
            lease_seconds: 3,
            heartbeat: Duration::from_secs(1),
            ..Default::default()
        },
        CancellationToken::new(),
    )?);
    service
        .execute(Dispatch {
            run_id,
            dispatch: run.dispatch,
        })
        .await
}
#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL; launches a real worker process"]
async fn force_killed_process_between_tools_recovers_only_the_uncommitted_action() -> Result<()> {
    killed_process(false).await
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL; launches a real worker process"]
async fn force_killed_pausing_process_never_resumes_automatically() -> Result<()> {
    killed_process(true).await
}

async fn killed_process(pausing: bool) -> Result<()> {
    let (service, first, second, _, run, url) = setup(false).await?;
    let mut inner = Arc::try_unwrap(service).ok().unwrap();
    inner.model = Arc::new(MockModel(AtomicUsize::new(1)));
    let service = Arc::new(inner);
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../outputs");
    std::fs::create_dir_all(&directory)?;
    let marker = directory.join(format!("process-fixture-{}.log", Uuid::new_v4()));
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args(["--ignored", "--exact", "fixture_process", "--nocapture"])
        .env("AGENT_FIXTURE_DATABASE", &url)
        .env("AGENT_FIXTURE_RUN", &run.id)
        .env("AGENT_FIXTURE_MARKER", &marker)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let started = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if std::fs::read_to_string(&marker)
                .unwrap_or_default()
                .contains("second")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    if started.is_ok() && pausing {
        let current = service.store.run(&run.id).await?;
        service
            .store
            .control(Control {
                run_id: run.id.clone(),
                expected_version: current.version,
                request_id: Uuid::new_v4().to_string(),
                action: ControlAction::Pause,
            })
            .await?;
    }
    child.kill()?;
    child.wait()?;
    started?;
    let checkpoint = service.store.run(&run.id).await?;
    assert_eq!(checkpoint.checkpoint.next_tool, 1);
    assert!(checkpoint.checkpoint.tool_inflight);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if service.store.recover(5).await? > 0
                || (pausing && service.store.run(&run.id).await?.status == RunStatus::Paused)
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await??;
    let recovered = service.store.run(&run.id).await?;
    if pausing {
        assert_eq!(recovered.status, RunStatus::Paused);
        assert_eq!(service.store.recover(5).await?, 0);
        service
            .clone()
            .execute(Dispatch {
                run_id: run.id.clone(),
                dispatch: recovered.dispatch,
            })
            .await?;
        assert_eq!(first.load(Ordering::SeqCst), 0);
        assert_eq!(second.load(Ordering::SeqCst), 0);
        assert_eq!(service.store.attempts(&run.id).await?.len(), 1);
        std::fs::remove_file(&marker)?;
        return cleanup(&url).await;
    }
    assert_eq!(recovered.status, RunStatus::Queued);
    service
        .clone()
        .execute(Dispatch {
            run_id: run.id.clone(),
            dispatch: recovered.dispatch,
        })
        .await?;
    let finished = service.store.run(&run.id).await?;
    assert_eq!(finished.status, RunStatus::Finished);
    assert_eq!(first.load(Ordering::SeqCst), 0);
    assert_eq!(second.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_to_string(&marker)?
            .lines()
            .filter(|line| *line == "first")
            .count(),
        1
    );
    assert_eq!(service.store.attempts(&run.id).await?.len(), 2);
    std::fs::remove_file(&marker)?;
    drop(service);
    cleanup(&url).await
}
