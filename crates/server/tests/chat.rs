use std::{collections::VecDeque, sync::Arc, time::Duration};

use agent::{ToolRegistry, agent_tool};
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use genai::{
    Client,
    resolver::{AuthData, AuthResolver, Endpoint, ServiceTargetResolver},
};
use http_body_util::BodyExt;
use opentelemetry::trace::TracerProvider;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use server::{AppState, default_registry, protocol::ChatInput, router};
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use tracing_subscriber::prelude::*;

#[tokio::test]
async fn observations_preserve_hierarchy_usage_parent_and_private_content() {
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    tracing::subscriber::set_global_default(subscriber).unwrap();
    {
        let mut f = fixture(
            vec![
                with_usage(tool("add", json!({"a":3,"b":5})), json!({"prompt_tokens":10})),
                with_usage(reply(json!({"content":"8"}), "stop"),json!({"prompt_tokens":10,"completion_tokens":4,"total_tokens":14,"prompt_tokens_details":{"cached_tokens":2},"completion_tokens_details":{"reasoning_tokens":1}})),
            ],
            6,
        )
        .await;
        f.state.telemetry = Arc::new(telemetry::Config {
            enabled: true,
            ..Default::default()
        });
        let response = router(f.state.clone(), vec![])
            .oneshot(
                Request::post("/api/chat")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        "traceparent",
                        "00-12345678901234567890123456789012-1234567890123456-01",
                    )
                    .body(Body::from(input("private text").to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let events = chunks(response).await;
        assert_eq!(
            events.last().unwrap()["messageMetadata"]["traceId"],
            "12345678901234567890123456789012"
        );
        f.state.tasks.close();
        f.state.tasks.wait().await;
    }
    provider.force_flush().unwrap();
    let spans: Vec<_> = exporter
        .get_finished_spans()
        .unwrap()
        .into_iter()
        .filter(|s| s.span_context.trace_id().to_string() == "12345678901234567890123456789012")
        .collect();
    let root = spans.iter().find(|s| s.name == "agent.run").unwrap();
    assert_eq!(root.parent_span_id.to_string(), "1234567890123456");
    assert!(
        root.attributes
            .iter()
            .any(|a| a.key.as_str() == "agent.outcome" && a.value.as_str() == "finished")
    );
    let steps: Vec<_> = spans.iter().filter(|s| s.name == "step").collect();
    assert_eq!(steps.len(), 2);
    for step in &steps {
        assert_eq!(step.parent_span_id, root.span_context.span_id());
    }
    assert_eq!(spans.iter().filter(|s| s.name == "model").count(), 2);
    assert_eq!(spans.iter().filter(|s| s.name == "tool").count(), 1);
    assert!(spans.iter().any(|s| {
        s.attributes
            .iter()
            .any(|a| a.key.as_str() == "llm.token_count.total" && a.value.as_str() == "14")
    }));
    assert!(
        spans
            .iter()
            .any(|s| s.attributes.iter().any(|a| a.key.as_str()
                == "llm.token_count.prompt_details.cache_read"
                && a.value.as_str() == "2"))
    );
    assert!(
        root.attributes
            .iter()
            .any(|a| a.key.as_str() == "agent.tokens.known_total" && a.value.as_str() == "14")
    );
    for s in &spans {
        assert!(
            !s.attributes
                .iter()
                .any(|a| matches!(a.key.as_str(), "input.value" | "output.value"))
        );
    }
    assert!(
        root.attributes
            .iter()
            .any(|a| a.key.as_str() == "agent.tokens.complete" && a.value.as_str() == "false")
    );
}

fn with_usage(body: String, usage: Value) -> String {
    body.replace("data: [DONE]", &format!("data: {}\n\ndata: [DONE]",json!({"id":"test","object":"chat.completion.chunk","created":0,"model":"gpt-4.1","choices":[],"usage":usage})))
}

#[tokio::test]
async fn trace_headers_and_run_source_validation() {
    let f = fixture(vec![reply(json!({"content":"hello"}), "stop")], 6).await;
    let app = router(
        f.state.clone(),
        vec!["http://localhost:3000".parse().unwrap()],
    );
    let invalid = app
        .clone()
        .oneshot(
            Request::post("/api/chat")
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-agent-run-source", "arbitrary-project")
                .body(Body::from(input("hello").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert!(f.requests.lock().await.is_empty());
    let options = app
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/chat")
                .header(header::ORIGIN, "http://localhost:3000")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(
                    header::ACCESS_CONTROL_REQUEST_HEADERS,
                    "content-type,traceparent,tracestate,x-agent-run-source",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let allowed = options.headers()[header::ACCESS_CONTROL_ALLOW_HEADERS]
        .to_str()
        .unwrap();
    for key in ["traceparent", "tracestate", "x-agent-run-source"] {
        assert!(allowed.contains(key));
    }
    let response = app
        .oneshot(
            Request::post("/api/chat")
                .header(header::CONTENT_TYPE, "application/json")
                .header("traceparent", "invalid")
                .body(Body::from(input("hello").to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(chunks(response).await.last().unwrap()["messageMetadata"]["runId"].is_string());
}

#[derive(Clone)]
struct Mock {
    replies: Arc<Mutex<VecDeque<String>>>,
    requests: Arc<Mutex<Vec<Value>>>,
    streaming: Arc<WaitContext>,
}

async fn respond(State(mock): State<Mock>, Json(request): Json<Value>) -> Response {
    mock.requests.lock().await.push(request);
    let body = mock
        .replies
        .lock()
        .await
        .pop_front()
        .expect("unexpected model request");
    if body == "stall" {
        let stream = async_stream::stream! {
            let _guard = DropSignal(mock.streaming.clone());
            mock.streaming.started.notify_one();
            yield Ok::<_, std::convert::Infallible>(axum::body::Bytes::from(delta(json!({"content":"partial"}), None)));
            std::future::pending::<()>().await;
        };
        return (
            [(header::CONTENT_TYPE, "text/event-stream")],
            Body::from_stream(stream),
        )
            .into_response();
    }
    ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}

struct Fixture {
    state: AppState,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
    streaming: Arc<WaitContext>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.shutdown.cancel();
        self.task.abort();
    }
}

async fn fixture(replies: Vec<String>, max_steps: usize) -> Fixture {
    let mock = Mock {
        replies: Arc::new(Mutex::new(replies.into())),
        requests: Arc::default(),
        streaming: Arc::new(WaitContext {
            started: Notify::new(),
            dropped: Notify::new(),
        }),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/", listener.local_addr().unwrap());
    let requests = mock.requests.clone();
    let streaming = mock.streaming.clone();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/chat/completions", post(respond))
                .with_state(mock),
        )
        .await
        .unwrap();
    });
    let client = Client::builder()
        .with_service_target_resolver(ServiceTargetResolver::from_resolver_fn(
            move |mut target: genai::ServiceTarget| {
                target.endpoint = Endpoint::from_owned(endpoint.clone());
                Ok(target)
            },
        ))
        .with_auth_resolver(AuthResolver::from_resolver_fn(|_| {
            Ok(Some(AuthData::from_single("test-key")))
        }))
        .build()
        .unwrap();
    Fixture {
        state: AppState {
            client,
            model: "openai::gpt-4.1".into(),
            max_steps,
            registry: Arc::new(default_registry().unwrap()),
            shutdown: CancellationToken::new(),
            telemetry: Arc::new(telemetry::Config::default()),
            tasks: tokio_util::task::TaskTracker::new(),
        },
        requests,
        task,
        streaming,
    }
}

fn delta(value: Value, finish: Option<&str>) -> String {
    let value = json!({"id":"test", "object":"chat.completion.chunk", "created":0, "model":"gpt-4.1", "choices":[{"index":0,"delta":value,"finish_reason":finish}]});
    format!("data: {value}\n\n")
}

fn reply(value: Value, finish: &str) -> String {
    format!(
        "{}{}data: [DONE]\n\n",
        delta(value, None),
        delta(json!({}), Some(finish))
    )
}

fn tool(name: &str, args: Value) -> String {
    reply(
        json!({"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":name,"arguments":args.to_string()}}]}),
        "tool_calls",
    )
}

fn input(text: &str) -> Value {
    json!({"id":"chat-1","trigger":"submit-message","messages":[{"id":"user-1","role":"user","parts":[{"type":"text","text":text}]}]})
}

async fn request(state: AppState, input: Value) -> Response {
    router(state, vec!["http://localhost:3000".parse().unwrap()])
        .oneshot(
            Request::post("/api/chat")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(input.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn chunks(response: Response) -> Vec<Value> {
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-vercel-ai-ui-message-stream"], "v1");
    assert!(
        response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "no-cache, no-transform"
    );
    let bytes = tokio::time::timeout(Duration::from_secs(5), response.into_body().collect())
        .await
        .unwrap()
        .unwrap()
        .to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(text.ends_with("data: [DONE]\n\n"));
    text.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|line| *line != "[DONE]")
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn text_stream_boundaries_and_isolated_concurrent_requests() {
    let f = fixture(
        vec![
            reply(json!({"content":"hello"}), "stop"),
            reply(json!({"content":"world"}), "stop"),
        ],
        6,
    )
    .await;
    let (a, b) = tokio::join!(
        request(f.state.clone(), input("first")),
        request(f.state.clone(), input("second"))
    );
    let (a, b) = tokio::join!(chunks(a), chunks(b));
    for events in [&a, &b] {
        assert_eq!(
            events
                .iter()
                .map(|event| event["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "start",
                "start-step",
                "text-start",
                "text-delta",
                "text-end",
                "finish-step",
                "finish"
            ]
        );
        assert_eq!(events[2]["id"], events[3]["id"]);
        assert_eq!(events[3]["id"], events[4]["id"]);
        assert_eq!(events.last().unwrap()["finishReason"], "stop");
    }
    assert_ne!(a[0]["messageId"], b[0]["messageId"]);
    let requests = f.requests.lock().await;
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request["messages"].as_array().unwrap().len() == 1)
    );
}

#[tokio::test]
async fn tool_success_failure_and_step_limit() {
    for (name, args, error) in [
        ("add", json!({"a":3,"b":5}), false),
        ("add", json!({"a":i64::MAX,"b":1}), true),
        ("missing", json!({}), true),
    ] {
        let f = fixture(vec![tool(name, args)], 1).await;
        let events = chunks(request(f.state.clone(), input("calculate")).await).await;
        assert_eq!(events[2]["type"], "tool-input-available");
        assert_eq!(
            events[3]["type"],
            if error {
                "tool-output-error"
            } else {
                "tool-output-available"
            }
        );
        assert_eq!(events[3]["toolCallId"], "call-1");
        if !error {
            assert_eq!(events[3]["output"], json!({"sum":8}));
        }
        let metadata = &events.last().unwrap()["messageMetadata"];
        assert_eq!(metadata["outcome"], "step-limit");
        assert_eq!(metadata["steps"], 1);
        assert!(metadata["runId"].as_str().is_some());
        assert!(metadata.get("traceId").is_none());
    }
    let f = fixture(
        vec![
            tool("add", json!({"a":3,"b":5})),
            reply(json!({"content":"8"}), "stop"),
        ],
        6,
    )
    .await;
    let events = chunks(request(f.state.clone(), input("calculate")).await).await;
    assert_eq!(
        events.iter().filter(|e| e["type"] == "start-step").count(),
        2
    );
    assert_eq!(
        events.iter().filter(|e| e["type"] == "finish-step").count(),
        2
    );
    let requests = f.requests.lock().await;
    assert_eq!(requests[1]["messages"][1]["tool_calls"][0]["id"], "call-1");
    assert_eq!(requests[1]["messages"][2]["tool_call_id"], "call-1");
}

#[tokio::test]
async fn second_turn_reconstructs_ordered_tool_history() {
    let f = fixture(vec![reply(json!({"content":"followup"}), "stop")], 6).await;
    let tool_part = json!({"type":"tool-add","state":"output-available","toolCallId":"previous-call","input":{"a":3,"b":5},"output":{"sum":8}});
    let dynamic = json!({"type":"dynamic-tool","toolName":"add","state":"output-error","toolCallId":"failed-call","input":{"a":i64::MAX,"b":1},"errorText":"overflow"});
    let mut body = input("next");
    body["messages"] = json!([
        {"id":"u1","role":"user","parts":[{"type":"text","text":"calculate"}]},
        {"id":"a1","role":"assistant","metadata":{"outcome":"finished","steps":3},"parts":[{"type":"step-start"},tool_part,{"type":"step-start"},dynamic,{"type":"step-start"},{"type":"text","text":"done"}]},
        {"id":"u2","role":"user","parts":[{"type":"text","text":"next"}]}
    ]);
    chunks(request(f.state.clone(), body).await).await;
    let requests = f.requests.lock().await;
    let messages = requests[0]["messages"].as_array().unwrap();
    assert_eq!(
        messages
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "user",
            "assistant",
            "tool",
            "assistant",
            "tool",
            "assistant",
            "user"
        ]
    );
    assert_eq!(messages[1]["tool_calls"][0]["id"], "previous-call");
    assert_eq!(messages[2]["content"], "{\"sum\":8}");
    assert_eq!(messages[4]["content"], "{\"error\":\"overflow\"}");
}

#[tokio::test]
async fn model_error_closes_partial_text_without_leaking_details() {
    let broken = format!(
        "{}data: {{broken-secret\n\n",
        delta(json!({"content":"partial"}), None)
    );
    let f = fixture(vec![broken], 6).await;
    let events = chunks(request(f.state.clone(), input("hello")).await).await;
    assert!(events.iter().any(|event| event["type"] == "text-end"));
    assert_eq!(
        events[events.len() - 2],
        json!({"type":"error","errorText":"Model request failed"})
    );
    assert_eq!(events.last().unwrap()["finishReason"], "error");
    assert!(
        !events
            .iter()
            .any(|event| event.to_string().contains("secret"))
    );
}

#[tokio::test]
async fn queue_overflow_terminates_with_an_error() {
    // All chunks arrive together, exceeding the synchronous callback queue in one poll.
    let mut body = (0..1024)
        .map(|_| delta(json!({"content":"x"}), None))
        .collect::<String>();
    body.push_str(&delta(json!({}), Some("stop")));
    body.push_str("data: [DONE]\n\n");
    let f = fixture(vec![body], 6).await;
    let events = chunks(request(f.state.clone(), input("hello")).await).await;
    assert!(events.iter().any(|event| {
        event["type"] == "error"
            && event["errorText"]
                .as_str()
                .unwrap()
                .contains("buffer exceeded")
    }));
    assert_eq!(events.last().unwrap()["finishReason"], "error");
}

#[tokio::test]
async fn invalid_requests_health_cors_and_body_limit() {
    let f = fixture(vec![], 6).await;
    let mut regenerate = input("hello");
    regenerate["trigger"] = json!("regenerate-message");
    let mut attachment = input("hello");
    attachment["messages"][0]["parts"] = json!([{"type":"file","url":"x","mediaType":"image/png"}]);
    let mut unfinished = input("hello");
    unfinished["messages"].as_array_mut().unwrap().insert(0, json!({"id":"a1","role":"assistant","parts":[{"type":"tool-add","state":"input-available","toolCallId":"x","input":{}}]}));
    for body in [regenerate, attachment, unfinished, json!({"messages":[]})] {
        assert_eq!(
            request(f.state.clone(), body).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert!(f.requests.lock().await.is_empty());
    let app = router(
        f.state.clone(),
        vec!["http://localhost:3000".parse().unwrap()],
    );
    let health = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    for origin in ["http://localhost:3000", "https://unexpected.example"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/api/chat")
                    .header(header::ORIGIN, origin)
                    .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                    .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "content-type")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_some(),
            origin == "http://localhost:3000"
        );
    }
    let response = request(f.state.clone(), input(&"a".repeat(2 * 1024 * 1024))).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[derive(Deserialize, JsonSchema)]
struct WaitArgs {}

struct WaitContext {
    started: Notify,
    dropped: Notify,
}
struct DropSignal(Arc<WaitContext>);
impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.dropped.notify_one();
    }
}

/// Wait forever to exercise cancellation.
#[agent_tool]
async fn wait(_args: WaitArgs, ctx: &Arc<WaitContext>) -> anyhow::Result<Value> {
    let _guard = DropSignal(ctx.clone());
    ctx.started.notify_one();
    std::future::pending().await
}

#[tokio::test]
async fn disconnect_and_shutdown_drop_running_tool() {
    for shutdown in [false, true] {
        let mut f = fixture(vec![tool("wait", json!({}))], 6).await;
        let ctx = Arc::new(WaitContext {
            started: Notify::new(),
            dropped: Notify::new(),
        });
        let mut registry = ToolRegistry::default();
        registry
            .register(WaitTool::new(Arc::new(ctx.clone())))
            .unwrap();
        f.state.registry = Arc::new(registry);
        let response = request(f.state.clone(), input("wait")).await;
        tokio::time::timeout(Duration::from_secs(5), ctx.started.notified())
            .await
            .unwrap();
        if shutdown {
            f.state.shutdown.cancel();
            let events = chunks(response).await;
            assert_eq!(events.last().unwrap()["type"], "abort");
        } else {
            drop(response);
        }
        tokio::time::timeout(Duration::from_secs(5), ctx.dropped.notified())
            .await
            .unwrap();
        assert_eq!(f.requests.lock().await.len(), 1);
    }
}

#[tokio::test]
async fn disconnect_drops_pending_model_stream() {
    let f = fixture(vec!["stall".into()], 6).await;
    let response = request(f.state.clone(), input("wait for model")).await;
    tokio::time::timeout(Duration::from_secs(5), f.streaming.started.notified())
        .await
        .unwrap();
    drop(response);
    tokio::time::timeout(Duration::from_secs(5), f.streaming.dropped.notified())
        .await
        .unwrap();
    assert_eq!(f.requests.lock().await.len(), 1);
}

#[tokio::test]
async fn idle_response_emits_comment_heartbeat() {
    let mut f = fixture(vec![tool("wait", json!({}))], 6).await;
    let ctx = Arc::new(WaitContext {
        started: Notify::new(),
        dropped: Notify::new(),
    });
    let mut registry = ToolRegistry::default();
    registry
        .register(WaitTool::new(Arc::new(ctx.clone())))
        .unwrap();
    f.state.registry = Arc::new(registry);
    let mut body = request(f.state.clone(), input("wait")).await.into_body();
    let heartbeat = tokio::time::timeout(Duration::from_secs(12), async {
        while let Some(frame) = body.frame().await {
            let frame = frame.unwrap();
            if let Ok(data) = frame.into_data()
                && String::from_utf8_lossy(&data).contains(": ping")
            {
                return;
            }
        }
        panic!("stream ended before heartbeat");
    })
    .await;
    assert!(heartbeat.is_ok());
    drop(body);
    tokio::time::timeout(Duration::from_secs(5), ctx.dropped.notified())
        .await
        .unwrap();
}

#[test]
fn invalid_history_rejects_duplicate_calls_and_unknown_parts() {
    for part in [
        json!({"type":"reasoning","text":"hidden"}),
        json!({"type":"tool-add","state":"output-available","toolCallId":"x","input":{}}),
    ] {
        let body = json!({"messages":[{"id":"a","role":"assistant","parts":[part]},{"id":"u","role":"user","parts":[{"type":"text","text":"next"}]}]});
        assert!(
            serde_json::from_value::<ChatInput>(body)
                .unwrap()
                .into_history()
                .is_err()
        );
    }
    let tool = json!({"type":"tool-add","state":"output-available","toolCallId":"duplicate","input":{},"output":{}});
    let body = json!({"messages":[{"id":"a","role":"assistant","parts":[tool.clone(),tool]},{"id":"u","role":"user","parts":[{"type":"text","text":"next"}]}]});
    assert!(
        serde_json::from_value::<ChatInput>(body)
            .unwrap()
            .into_history()
            .unwrap_err()
            .to_string()
            .contains("unique")
    );
}
