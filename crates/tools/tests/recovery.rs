use futures::future::BoxFuture;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tools::{AgentTool, ExecutionContext, RecoveryPolicy, SafeToRetry};

struct Local;
impl AgentTool for Local {
    fn name(&self) -> &'static str {
        "local"
    }
    fn description(&self) -> &'static str {
        "local computation"
    }
    fn schema(&self) -> Value {
        json!({})
    }
    fn execute(&self, args: Value) -> BoxFuture<'_, anyhow::Result<Value>> {
        Box::pin(async move { Ok(args) })
    }
}

#[tokio::test]
async fn recovery_is_explicit_and_context_keeps_the_original_operation() {
    assert_eq!(Local.recovery_policy(), RecoveryPolicy::Conservative);
    assert_eq!(
        SafeToRetry(Local).recovery_policy(),
        RecoveryPolicy::SafeToRetry
    );
    let saved = Arc::new(Mutex::new(None));
    let recorder = saved.clone();
    let context = ExecutionContext::new(
        "run/call-original".into(),
        None,
        Arc::new(move |id| {
            let recorder = recorder.clone();
            Box::pin(async move {
                *recorder.lock().unwrap() = Some(id);
                Ok(())
            })
        }),
    );
    context
        .record_external_id("external-job".into())
        .await
        .unwrap();
    assert_eq!(saved.lock().unwrap().as_deref(), Some("external-job"));
    assert_eq!(context.operation_key, "run/call-original");
    assert!(Local.reconcile(json!({}), &context).await.is_err());
    assert_eq!(
        Local
            .execute_with_context(json!({"sum":3}), &context)
            .await
            .unwrap(),
        json!({"sum":3})
    );
}
