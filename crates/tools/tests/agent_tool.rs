use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tools::{AgentTool, agent_tool};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Args {
    /// Number to add.
    amount: usize,
}

#[derive(Debug, Serialize, PartialEq)]
struct Output {
    total: usize,
}

/// Add one to the input.
/// Returns a structured result.
#[agent_tool]
async fn increment(args: Args) -> anyhow::Result<Output> {
    Ok(Output {
        total: args.amount + 1,
    })
}

#[agent_tool(name = "echo-value", description = "Return JSON unchanged.")]
async fn echo(args: Args) -> Result<Value, std::io::Error> {
    Ok(json!({"amount": args.amount}))
}

struct Counter {
    value: AtomicUsize,
    calls: AtomicUsize,
}

/// Add to the shared counter.
#[agent_tool(name = "counter")]
async fn add_to_counter(args: Args, ctx: &Counter) -> anyhow::Result<Output> {
    ctx.calls.fetch_add(1, Ordering::SeqCst);
    // Ensure the borrowed context survives an await point.
    tokio::task::yield_now().await;
    let previous = ctx.value.fetch_add(args.amount, Ordering::SeqCst);
    Ok(Output {
        total: previous + args.amount,
    })
}

#[agent_tool(description = "Always fails.")]
async fn fails(_args: Args) -> Result<Value, std::io::Error> {
    Err(std::io::Error::other("backend unavailable"))
}

#[tokio::test]
async fn defaults_schema_and_original_function() {
    let tool = IncrementTool;
    assert_eq!(tool.name(), "increment");
    assert_eq!(
        tool.description(),
        "Add one to the input.\nReturns a structured result."
    );
    let schema = tool.schema();
    assert_eq!(schema["properties"]["amount"]["type"], "integer");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"], json!(["amount"]));
    assert_eq!(
        tool.execute(json!({"amount": 4})).await.unwrap(),
        json!({"total": 5})
    );
    assert_eq!(
        increment(Args { amount: 8 }).await.unwrap(),
        Output { total: 9 }
    );
}

#[tokio::test]
async fn overrides_and_value_output() {
    assert_eq!(EchoTool.name(), "echo-value");
    assert_eq!(EchoTool.description(), "Return JSON unchanged.");
    assert_eq!(
        EchoTool.execute(json!({"amount": 3})).await.unwrap(),
        json!({"amount": 3})
    );
    assert_eq!(FailsTool.name(), "fails");
    assert_eq!(FailsTool.description(), "Always fails.");
}

#[tokio::test]
async fn shared_state_and_invalid_arguments() {
    let ctx = Arc::new(Counter {
        value: AtomicUsize::new(10),
        calls: AtomicUsize::new(0),
    });
    let first = AddToCounterTool::new(ctx.clone());
    let second = AddToCounterTool::new(ctx.clone());
    assert_eq!(first.name(), "counter");
    assert_eq!(first.description(), "Add to the shared counter.");
    let tools: Vec<Box<dyn AgentTool>> = vec![Box::new(first), Box::new(second)];
    assert_eq!(
        tools[0].execute(json!({"amount": 2})).await.unwrap(),
        json!({"total": 12})
    );
    assert_eq!(
        tools[1].execute(json!({"amount": 3})).await.unwrap(),
        json!({"total": 15})
    );
    for args in [
        json!({}),
        json!({"amount": "bad"}),
        json!({"amount": 1, "unknown": true}),
    ] {
        let error = tools[0].execute(args).await.unwrap_err();
        assert!(error.to_string().contains("invalid arguments for counter"));
    }
    assert_eq!(ctx.calls.load(Ordering::SeqCst), 2);
    let isolated = AddToCounterTool::new(Arc::new(Counter {
        value: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
    }));
    assert_eq!(
        isolated.execute(json!({"amount": 1})).await.unwrap(),
        json!({"total": 1})
    );
}

#[tokio::test]
async fn propagates_function_and_serialization_errors() {
    let error = FailsTool.execute(json!({"amount": 1})).await.unwrap_err();
    assert_eq!(error.to_string(), "backend unavailable");
    assert!(
        BadOutputTool
            .execute(json!({"amount": 1}))
            .await
            .unwrap_err()
            .to_string()
            .contains("serialization failed")
    );
}

struct Unserializable;
impl Serialize for Unserializable {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("serialization failed"))
    }
}

/// Exercise a serializer failure.
#[agent_tool]
async fn bad_output(_: Args) -> anyhow::Result<Unserializable> {
    Ok(Unserializable)
}

mod hygiene {
    use super::*;

    /// Return a value even when the function name matches an internal variable.
    #[agent_tool]
    async fn args(args: Args) -> anyhow::Result<usize> {
        Ok(args.amount)
    }

    #[tokio::test]
    async fn function_name_does_not_collide_with_generated_argument() {
        assert_eq!(
            ArgsTool.execute(json!({"amount": 7})).await.unwrap(),
            json!(7)
        );
    }
}
