//! Typed tools and the `#[agent_tool]` function attribute.
extern crate self as tools;

use futures::future::BoxFuture;
use serde_json::Value;
pub use tool_macros::agent_tool;

pub trait AgentTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn schema(&self) -> Value;
    fn execute(&self, arguments: Value) -> BoxFuture<'_, anyhow::Result<Value>>;
}

/// Dependencies referenced by generated code, including with renamed dependencies.
#[doc(hidden)]
pub mod __private {
    pub use anyhow;
    pub use futures;
    pub use schemars;
    pub use serde_json;
}
