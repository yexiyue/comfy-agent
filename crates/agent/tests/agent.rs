use agent::{AgentEvent, AgentOutcome, ToolRegistry, agent_tool, run_agent};
use genai::{
    Client,
    chat::{ChatRequest, ToolCall},
    resolver::{AuthData, AuthResolver, Endpoint, ServiceTargetResolver},
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[derive(Deserialize, JsonSchema)]
struct Args {
    value: i64,
}

/// Double an integer.
#[agent_tool]
async fn double(args: Args) -> anyhow::Result<Value> {
    Ok(json!({"result": args.value * 2}))
}

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        call_id: "call_1".into(),
        fn_name: name.into(),
        fn_arguments: arguments,
        thought_signatures: None,
    }
}

#[tokio::test]
async fn registry_dispatch_and_validation() {
    let mut registry = ToolRegistry::default();
    registry.register(DoubleTool).unwrap();
    assert!(registry.register(DoubleTool).is_err());
    assert_eq!(registry.definitions().len(), 1);
    assert_eq!(
        registry
            .execute(&call("double", json!({"value": 4})))
            .await
            .unwrap(),
        json!({"result": 8})
    );
    assert!(registry.execute(&call("missing", json!({}))).await.is_err());
    assert!(
        registry
            .execute(&call("double", json!({"value": "bad"})))
            .await
            .is_err()
    );
}

fn stream(delta: Value, finish: &str) -> String {
    let event = json!({"id":"test", "object":"chat.completion.chunk", "created":0, "model":"gpt-4.1",
        "choices":[{"index":0,"delta":delta,"finish_reason":null}]});
    let end = json!({"id":"test", "object":"chat.completion.chunk", "created":0, "model":"gpt-4.1",
        "choices":[{"index":0,"delta":{},"finish_reason":finish}]});
    format!("data: {event}\n\ndata: {end}\n\ndata: [DONE]\n\n")
}

async fn mock_client(replies: Vec<String>) -> (Client, tokio::task::JoinHandle<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for body in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut chunk = [0; 4096];
                let read = socket.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0);
                bytes.extend_from_slice(&chunk[..read]);
                if let Some(index) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let size: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            while bytes.len() < header_end + size {
                let mut chunk = [0; 4096];
                let read = socket.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0);
                bytes.extend_from_slice(&chunk[..read]);
            }
            requests.push(serde_json::from_slice(&bytes[header_end..header_end + size]).unwrap());
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        }
        requests
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
    (client, server)
}

async fn turn(
    max_steps: usize,
    name: &str,
) -> (AgentOutcome, Vec<AgentEvent>, ChatRequest, Vec<Value>) {
    let tool = stream(
        json!({"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":name,"arguments":"{\"value\":4}"}}]}),
        "tool_calls",
    );
    let mut replies = vec![tool];
    if max_steps > 1 {
        replies.push(stream(json!({"content":"8"}), "stop"));
    }
    let (client, server) = mock_client(replies).await;
    let mut history = ChatRequest::from_user("double 4");
    let mut registry = ToolRegistry::default();
    registry.register(DoubleTool).unwrap();
    let mut events = Vec::new();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        run_agent(
            &client,
            "openai::gpt-4.1",
            &mut history,
            &registry,
            max_steps,
            |event| events.push(event),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    let requests = server.await.unwrap();
    (outcome, events, history, requests)
}

#[tokio::test]
async fn tool_roundtrip_preserves_history_and_emits_events() {
    let (outcome, events, history, requests) = turn(2, "double").await;
    assert_eq!(
        outcome,
        AgentOutcome::Finished {
            answer: "8".into(),
            steps: 2
        }
    );
    assert_eq!(history.messages.len(), 4);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::StepFinished { .. }))
            .count(),
        2
    );
    assert!(matches!(
        events.last(),
        Some(AgentEvent::StepFinished { step: 2 })
    ));
    assert!(matches!(
        events.first(),
        Some(AgentEvent::StepStarted { step: 1, .. })
    ));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::TextDelta(text) if text == "8"))
    );
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolFinished { value, is_error: false, .. } if value == &json!({"result":8}))));
    assert_eq!(requests[1]["messages"][1]["role"], "assistant");
    assert_eq!(requests[1]["messages"][2]["role"], "tool");
    assert_eq!(requests[1]["messages"][2]["tool_call_id"], "call_1");
    assert!(
        requests
            .iter()
            .all(|request| request["tools"].as_array().unwrap().len() == 1)
    );
}

#[tokio::test]
async fn step_limit_and_tool_error_keep_complete_tool_exchange() {
    let (outcome, events, history, _) = turn(1, "missing").await;
    assert_eq!(outcome, AgentOutcome::StepLimit { steps: 1 });
    assert_eq!(history.messages.len(), 3);
    assert!(matches!(
        events.last(),
        Some(AgentEvent::StepFinished { step: 1 })
    ));
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ToolFinished { value, is_error: true, .. } if value["error"].is_string())));
}

#[tokio::test]
async fn zero_steps_rejected_before_model_request() {
    let client = Client::new().unwrap();
    let mut history = ChatRequest::from_user("hello");
    let error = run_agent(
        &client,
        "unused",
        &mut history,
        &ToolRegistry::default(),
        0,
        |_| {},
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("greater than 0"));
    assert_eq!(history.messages.len(), 1);
}
