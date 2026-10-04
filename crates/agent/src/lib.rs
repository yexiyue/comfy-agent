//! Agent execution and tool dispatch, independent of the user interface.
pub mod agent;
pub mod config;
mod llm;
pub mod tool_registry;

pub use agent::{AgentEvent, AgentOutcome, run_agent};
pub use config::AgentConfig;
pub use tool_registry::ToolRegistry;
pub use tools::{AgentTool, agent_tool};
