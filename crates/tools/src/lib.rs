//! Typed tools and the `#[agent_tool]` function attribute.
extern crate self as tools;

use futures::future::BoxFuture;
use serde_json::Value;
use std::sync::Arc;
pub use tool_macros::agent_tool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryPolicy {
    SafeToRetry,
    Idempotent,
    Reconcilable,
    Conservative,
}

impl RecoveryPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SafeToRetry => "safe-to-retry",
            Self::Idempotent => "idempotent",
            Self::Reconcilable => "reconcilable",
            Self::Conservative => "conservative",
        }
    }
}

pub type ExternalIdRecorder =
    Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// Stable operation identity survives retries; the recorder checks writer ownership.
pub struct ExecutionContext {
    pub operation_key: String,
    pub external_id: Option<String>,
    recorder: ExternalIdRecorder,
}

impl ExecutionContext {
    pub fn new(
        operation_key: String,
        external_id: Option<String>,
        recorder: ExternalIdRecorder,
    ) -> Self {
        Self {
            operation_key,
            external_id,
            recorder,
        }
    }
    pub async fn record_external_id(&self, id: String) -> anyhow::Result<()> {
        anyhow::ensure!(
            !id.trim().is_empty() && id.len() <= 1024,
            "invalid external operation ID"
        );
        anyhow::ensure!(
            self.external_id
                .as_deref()
                .is_none_or(|existing| existing == id),
            "external operation ID changed"
        );
        (self.recorder)(id).await
    }
}

pub trait AgentTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn schema(&self) -> Value;
    fn execute(&self, arguments: Value) -> BoxFuture<'_, anyhow::Result<Value>>;
    fn recovery_policy(&self) -> RecoveryPolicy {
        RecoveryPolicy::Conservative
    }
    fn execute_with_context<'a>(
        &'a self,
        arguments: Value,
        _context: &'a ExecutionContext,
    ) -> BoxFuture<'a, anyhow::Result<Value>> {
        self.execute(arguments)
    }
    fn reconcile<'a>(
        &'a self,
        _arguments: Value,
        _context: &'a ExecutionContext,
    ) -> BoxFuture<'a, anyhow::Result<Value>> {
        Box::pin(async { anyhow::bail!("tool has no external reconciliation adapter") })
    }
}

/// Explicit opt-in for tools whose incomplete action can safely be repeated.
pub struct SafeToRetry<T>(pub T);
impl<T: AgentTool> AgentTool for SafeToRetry<T> {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn description(&self) -> &'static str {
        self.0.description()
    }
    fn schema(&self) -> Value {
        self.0.schema()
    }
    fn execute(&self, arguments: Value) -> BoxFuture<'_, anyhow::Result<Value>> {
        self.0.execute(arguments)
    }
    fn recovery_policy(&self) -> RecoveryPolicy {
        RecoveryPolicy::SafeToRetry
    }
    fn execute_with_context<'a>(
        &'a self,
        arguments: Value,
        context: &'a ExecutionContext,
    ) -> BoxFuture<'a, anyhow::Result<Value>> {
        self.0.execute_with_context(arguments, context)
    }
}

/// Dependencies referenced by generated code, including with renamed dependencies.
#[doc(hidden)]
pub mod __private {
    pub use anyhow;
    pub use futures;
    pub use schemars;
    pub use serde_json;
}
