//! Durable routes use an isolated real PostgreSQL database and free local mock models.
#[path = "../../persistence/tests/common/mod.rs"]
mod common;
use anyhow::Result;
use axum::{
    body::Body,
    http::{Request, StatusCode, header},
    response::Response,
};
use futures::future::BoxFuture;
use genai::chat::{ChatRequest, MessageContent};
use http_body_util::BodyExt;
use runtime::{
    execution::{ExecutionService, ModelGateway, WorkerConfig},
    model::*,
};
use serde_json::{Value, json};
use server::{AppState, default_registry, router};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
struct Model {
    calls: Arc<AtomicUsize>,
    block: bool,
}
impl ModelGateway for Model {
    fn response<'a>(
        &'a self,
        _model: &'a str,
        request: ChatRequest,
        text: &'a mut (dyn FnMut(String) + Send),
    ) -> BoxFuture<'a, Result<agent::ModelResponse>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert!(!request.messages.is_empty());
            text("hello".into());
            if self.block {
                std::future::pending::<()>().await;
            }
            Ok(agent::ModelResponse {
                content: MessageContent::from_text("hello"),
                usage: None,
                stop_reason: None,
            })
        })
    }
}
async fn fixture(block: bool) -> Result<(AppState, Arc<AtomicUsize>, String)> {
    let url = common::database().await?;
    let mut db = persistence::connect(&url).await?;
    persistence::migration::migrate(&mut db).await?;
    let store = Arc::new(persistence::repository::PostgresStore::open(&url).await?);
    let calls = Arc::new(AtomicUsize::new(0));
    let service = Arc::new(ExecutionService::new(
        store,
        Arc::new(Model {
            calls: calls.clone(),
            block,
        }),
        Arc::new(default_registry()?),
        WorkerConfig {
            heartbeat: Duration::from_millis(50),
            ..Default::default()
        },
        CancellationToken::new(),
    )?);
    Ok((
        AppState {
            service,
            model: "mock".into(),
            max_steps: 2,
            tool_schema_hash: "mock".into(),
        },
        calls,
        url,
    ))
}
async fn request(state: &AppState, path: &str, body: Option<Value>) -> Response {
    let mut builder = if body.is_some() {
        Request::post(path)
    } else {
        Request::get(path)
    };
    builder = builder.header(header::CONTENT_TYPE, "application/json");
    router(
        state.clone(),
        vec!["http://localhost:5173".parse().unwrap()],
    )
    .oneshot(
        builder
            .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
            .unwrap(),
    )
    .await
    .unwrap()
}
async fn value(response: Response) -> Value {
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}
fn message(id: &str) -> Value {
    json!({"id":id,"role":"user","parts":[{"type":"text","text":"hi"}]})
}
async fn conversation(state: &AppState) -> String {
    value(request(state, "/api/conversations", Some(json!({}))).await).await["id"]
        .as_str()
        .unwrap()
        .into()
}
async fn submit(state: &AppState, id: &str) -> (Response, Run) {
    let response = request(
        state,
        "/api/chat",
        Some(
            json!({"id":id,"expectedRevision":0,"requestId":"submission","message":message("u1")}),
        ),
    )
    .await;
    let c = state.service.store.conversation(id).await.unwrap();
    let run = state
        .service
        .store
        .run(c.active_run_id.as_deref().unwrap())
        .await
        .unwrap();
    (response, run)
}
async fn chunks(response: Response) -> Vec<Value> {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.contains("data: [DONE]"));
    text.lines()
        .filter_map(|line| {
            line.strip_prefix("data: ")
                .and_then(|line| serde_json::from_str(line).ok())
        })
        .collect()
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL; exercises the real 30-second slow-consumer bound"]
async fn slow_consumer_detaches_without_stopping_the_background_run() -> Result<()> {
    let (state, calls, url) = fixture(false).await?;
    let id = conversation(&state).await;
    let (response, run) = submit(&state, &id).await;
    state
        .service
        .clone()
        .execute(Dispatch {
            run_id: run.id.clone(),
            dispatch: run.dispatch,
        })
        .await?;
    let mut body = response.into_body();
    let mut prefix = String::new();
    while !prefix.contains("text-delta") {
        let frame = body.frame().await.unwrap()?;
        if let Ok(data) = frame.into_data() {
            prefix.push_str(&String::from_utf8(data.to_vec())?);
        }
    }
    tokio::time::sleep(Duration::from_secs(31)).await;
    let tail = String::from_utf8(body.collect().await?.to_bytes().to_vec())?;
    assert!(tail.contains("abort") && tail.contains("text-end") && tail.contains("finish-step"));
    assert!(!tail.contains("\"type\":\"finish\""));
    assert_eq!(
        state.service.store.run(&run.id).await?.status,
        RunStatus::Finished
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let replay = chunks(request(&state, &format!("/api/chat/{}/stream", run.id), None).await).await;
    assert!(replay.iter().any(|e| e["type"] == "finish"));
    common::cleanup(&url).await
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL"]
async fn storage_failure_aborts_only_the_subscription_and_stops_inflight_execution() -> Result<()> {
    let (state, calls, url) = fixture(true).await?;
    let id = conversation(&state).await;
    let (response, run) = submit(&state, &id).await;
    let execution = tokio::spawn(state.service.clone().execute(Dispatch {
        run_id: run.id.clone(),
        dispatch: run.dispatch,
    }));
    tokio::time::timeout(Duration::from_secs(5), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    // Fail only this generated database; restore the table before cleaning up.
    let mut db = persistence::connect(&url).await?;
    toasty::sql::statement("ALTER TABLE agent_runs RENAME TO temporarily_unavailable_runs")
        .exec(&mut db)
        .await?;
    let events = chunks(response).await;
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "error" && e["errorText"] == "Storage unavailable")
    );
    assert!(events.iter().any(|e| e["type"] == "abort"));
    assert_eq!(
        request(&state, &format!("/api/runs/{}", run.id), None)
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    let result = tokio::time::timeout(Duration::from_secs(5), execution).await??;
    toasty::sql::statement("ALTER TABLE temporarily_unavailable_runs RENAME TO agent_runs")
        .exec(&mut db)
        .await?;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        state.service.store.run(&run.id).await?.status,
        RunStatus::Running
    );
    common::cleanup(&url).await
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL"]
async fn authoritative_snapshots_idempotent_submission_replay_and_second_turn() -> Result<()> {
    let (state, calls, url) = fixture(false).await?;
    let id = conversation(&state).await;
    let (response, run) = submit(&state, &id).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-vercel-ai-ui-message-stream"], "v1");
    state
        .service
        .clone()
        .execute(Dispatch {
            run_id: run.id.clone(),
            dispatch: run.dispatch,
        })
        .await?;
    let events = chunks(response).await;
    assert_eq!(events[0]["messageId"], run.assistant_id);
    assert!(events.iter().any(|e| e["type"] == "finish"));
    let replay = chunks(request(&state, &format!("/api/chat/{}/stream", run.id), None).await).await;
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] != "data-run-state")
            .collect::<Vec<_>>(),
        replay
            .iter()
            .filter(|e| e["type"] != "data-run-state")
            .collect::<Vec<_>>()
    );
    let snapshot = state.service.store.conversation(&id).await?;
    assert_eq!(snapshot.messages.len(), 2);
    assert_eq!(snapshot.history.messages.len(), 2);
    assert_eq!(snapshot.messages[1].parts[1]["text"], "hello");
    let retry = request(
        &state,
        "/api/chat",
        Some(
            json!({"id":id,"expectedRevision":0,"requestId":"submission","message":message("u1")}),
        ),
    )
    .await;
    assert_eq!(retry.status(), StatusCode::OK);
    drop(retry);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let second=request(&state,"/api/chat",Some(json!({"id":id,"expectedRevision":snapshot.revision,"requestId":"second","message":message("u2")}))).await;
    assert_eq!(second.status(), StatusCode::OK);
    drop(second);
    state.service.shutdown.cancel();
    drop(state);
    common::cleanup(&url).await
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL"]
async fn invalid_commands_cors_body_limit_and_health_do_not_call_model() -> Result<()> {
    let (state, calls, url) = fixture(false).await?;
    let id = conversation(&state).await;
    for body in [
        json!({"id":id,"messages":[message("u1")]}),
        json!({"id":id,"expectedRevision":0,"requestId":"bad","message":message("u1"),"trigger":"regenerate-message"}),
        json!({"id":id,"expectedRevision":0,"requestId":"bad","message":{"id":"u1","role":"user","parts":[{"type":"file"}]}}),
    ] {
        assert_eq!(
            request(&state, "/api/chat", Some(body)).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        request(&state, "/api/runs/missing", None).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        request(&state, "/health", None).await.status(),
        StatusCode::OK
    );
    let bad = request(
        &state,
        "/api/chat",
        Some(json!({"padding":"x".repeat(2*1024*1024)})),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let cors = router(
        state.clone(),
        vec!["http://localhost:5173".parse().unwrap()],
    )
    .oneshot(
        Request::builder()
            .method("OPTIONS")
            .uri("/api/chat")
            .header("origin", "http://localhost:5173")
            .header("access-control-request-method", "POST")
            .body(Body::empty())?,
    )
    .await?;
    assert_eq!(
        cors.headers()["access-control-allow-origin"],
        "http://localhost:5173"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    state.service.shutdown.cancel();
    drop(state);
    common::cleanup(&url).await
}
#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL"]
async fn disconnect_only_detaches_and_explicit_pause_cancel_are_fenced() -> Result<()> {
    let (state, calls, url) = fixture(true).await?;
    let id = conversation(&state).await;
    let (response, run) = submit(&state, &id).await;
    let execution = tokio::spawn(state.service.clone().execute(Dispatch {
        run_id: run.id.clone(),
        dispatch: run.dispatch,
    }));
    drop(response);
    tokio::time::timeout(Duration::from_secs(3), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert_eq!(
        state.service.store.run(&run.id).await?.status,
        RunStatus::Running
    );
    let current = state.service.store.run(&run.id).await?;
    let body = json!({"conversationId":id,"expectedVersion":current.version,"requestId":"pause"});
    assert_eq!(
        request(
            &state,
            &format!("/api/runs/{}/pause", run.id),
            Some(body.clone())
        )
        .await
        .status(),
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(3), execution).await???;
    let paused = state.service.store.run(&run.id).await?;
    assert_eq!(paused.status, RunStatus::Paused);
    assert_eq!(
        request(&state, &format!("/api/runs/{}/pause", run.id), Some(body))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(request(&state,&format!("/api/runs/{}/resume",run.id),Some(json!({"conversationId":"other","expectedVersion":paused.version,"requestId":"wrong"}))).await.status(),StatusCode::NOT_FOUND);
    assert_eq!(
        request(
            &state,
            &format!("/api/runs/{}/cancel", run.id),
            Some(
                json!({"conversationId":id,"expectedVersion":paused.version,"requestId":"cancel"})
            )
        )
        .await
        .status(),
        StatusCode::OK
    );
    assert_eq!(
        state.service.store.run(&run.id).await?.status,
        RunStatus::Cancelled
    );
    state.service.shutdown.cancel();
    drop(state);
    common::cleanup(&url).await
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL"]
async fn memory_exporter_correlates_attempts_and_keeps_disconnect_distinct_from_pause() -> Result<()>
{
    use opentelemetry::trace::TracerProvider;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use tracing_subscriber::prelude::*;
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    tracing::subscriber::set_global_default(
        tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::sink)
                    .with_filter(tracing_subscriber::EnvFilter::new("info")),
            )
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("durable-test"))),
    )?;
    let (mut state, _, url) = fixture(false).await?;
    Arc::get_mut(&mut state.service).unwrap().telemetry = Arc::new(telemetry::Config {
        enabled: true,
        ..Default::default()
    });
    let first = conversation(&state).await;
    let second = conversation(&state).await;
    let response=router(state.clone(),vec![]).oneshot(Request::post("/api/chat").header("content-type","application/json").header("traceparent","00-12345678901234567890123456789012-1234567890123456-01").body(Body::from(json!({"id":first,"expectedRevision":0,"requestId":"parent","message":message("u1")}).to_string()))?).await?;
    let a = state
        .service
        .store
        .run(
            state
                .service
                .store
                .conversation(&first)
                .await?
                .active_run_id
                .as_deref()
                .unwrap(),
        )
        .await?;
    assert_eq!(
        a.traceparent.as_deref(),
        Some("00-12345678901234567890123456789012-1234567890123456-01")
    );
    drop(response);
    let (response, b) = submit(&state, &second).await;
    drop(response);
    let (a_result, b_result) = tokio::join!(
        state.service.clone().execute(Dispatch {
            run_id: a.id.clone(),
            dispatch: a.dispatch
        }),
        state.service.clone().execute(Dispatch {
            run_id: b.id.clone(),
            dispatch: b.dispatch
        })
    );
    a_result?;
    b_result?;
    let a_attempt = state.service.store.attempts(&a.id).await?.remove(0);
    let b_attempt = state.service.store.attempts(&b.id).await?.remove(0);
    assert_ne!(a_attempt.trace_id, b_attempt.trace_id);
    assert_eq!(
        a_attempt.trace_id.as_deref(),
        Some("12345678901234567890123456789012")
    );
    assert_eq!(a_attempt.outcome.as_deref(), Some("finished"));
    let counters = state.service.store.attempts(&a.id).await?;
    chunks(request(&state, &format!("/api/chat/{}/stream", a.id), None).await).await;
    assert_eq!(
        serde_json::to_value(state.service.store.attempts(&a.id).await?)?,
        serde_json::to_value(counters)?
    );
    state.service.shutdown.cancel();
    drop(state);
    common::cleanup(&url).await?;
    let (mut state, calls, url) = fixture(true).await?;
    Arc::get_mut(&mut state.service).unwrap().telemetry = Arc::new(telemetry::Config {
        enabled: true,
        ..Default::default()
    });
    let id = conversation(&state).await;
    let (response, run) = submit(&state, &id).await;
    drop(response);
    let execution = tokio::spawn(state.service.clone().execute(Dispatch {
        run_id: run.id.clone(),
        dispatch: run.dispatch,
    }));
    tokio::time::timeout(Duration::from_secs(3), async {
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let current = state.service.store.run(&run.id).await?;
    state
        .service
        .control(Control {
            run_id: run.id.clone(),
            expected_version: current.version,
            request_id: "pause-observation".into(),
            action: ControlAction::Pause,
        })
        .await?;
    execution.await??;
    let paused = state.service.store.run(&run.id).await?;
    assert_eq!(paused.status, RunStatus::Paused);
    let attempt = state.service.store.attempts(&run.id).await?.remove(0);
    assert_eq!(attempt.outcome.as_deref(), Some("paused"));
    assert!(!attempt.usage_complete);
    state.service.shutdown.cancel();
    drop(state);
    common::cleanup(&url).await?;
    provider.force_flush()?;
    let spans = exporter.get_finished_spans()?;
    let run_ids = [&a.id, &b.id, &run.id];
    let attempt_ids = [&a_attempt.id, &b_attempt.id, &attempt.id];
    let roots: Vec<_> = spans
        .iter()
        .filter(|span| {
            span.name == "agent.run"
                && span.attributes.iter().any(|attribute| {
                    attribute.key.as_str() == "agent.run_id"
                        && run_ids
                            .iter()
                            .any(|id| attribute.value.as_str() == id.as_str())
                })
        })
        .collect();
    assert_eq!(roots.len(), 3);
    let outcomes: Vec<_> = roots
        .iter()
        .flat_map(|span| span.attributes.iter())
        .filter(|attribute| attribute.key.as_str() == "agent.outcome")
        .map(|attribute| attribute.value.as_str())
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|value| **value == "finished")
            .count(),
        2
    );
    assert_eq!(
        outcomes.iter().filter(|value| **value == "paused").count(),
        1
    );
    assert!(!outcomes.iter().any(|value| *value == "cancelled"));
    assert_eq!(
        spans
            .iter()
            .filter(|span| span.name == "agent.attempt"
                && span
                    .attributes
                    .iter()
                    .any(|attribute| attribute.key.as_str() == "agent.attempt_id"
                        && attempt_ids
                            .iter()
                            .any(|id| attribute.value.as_str() == id.as_str())))
            .count(),
        3
    );
    for span in &spans {
        assert!(
            !span
                .attributes
                .iter()
                .any(|attribute| matches!(attribute.key.as_str(), "input.value" | "output.value"))
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires dedicated TEST_DATABASE_URL"]
async fn strict_import_and_pagination_preserve_completed_multi_step_history() -> Result<()> {
    let (state, calls, url) = fixture(false).await?;
    let imported=request(&state,"/api/conversations",Some(json!({"messages":[{"id":"system","role":"system","parts":[{"type":"text","text":"be concise"}]},{"id":"user","role":"user","parts":[{"type":"text","text":"calculate"}]},{"id":"assistant","role":"assistant","parts":[{"type":"step-start"},{"type":"tool-add","toolCallId":"original-call","state":"output-error","input":{"a":1,"b":2},"errorText":"failed"},{"type":"step-start"},{"type":"text","text":"recover"}]}]}))).await;
    assert_eq!(imported.status(), StatusCode::OK);
    let imported_value = value(imported).await;
    let snapshot = state
        .service
        .store
        .conversation(imported_value["id"].as_str().unwrap())
        .await?;
    assert_eq!(snapshot.messages.len(), 3);
    assert_eq!(snapshot.history.messages.len(), 5);
    assert!(serde_json::to_string(&snapshot.history)?.contains("original-call"));
    for messages in [
        json!([message("duplicate"), message("duplicate")]),
        json!([{"id":"u","role":"user","parts":[{"type":"file","url":"invalid"}]}]),
        json!([{"id":"a","role":"assistant","parts":[{"type":"tool-add","toolCallId":"pending","state":"input-available","input":{}}]}]),
    ] {
        assert_eq!(
            request(
                &state,
                "/api/conversations",
                Some(json!({"messages":messages}))
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        request(&state, "/api/conversations?limit=101", None)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&state, "/api/conversations?offset=-1", None)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        value(request(&state, "/api/conversations?limit=1", None).await)
            .await
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(state);
    common::cleanup(&url).await
}
