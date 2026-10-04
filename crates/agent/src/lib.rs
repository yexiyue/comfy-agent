//! Agent execution and tool dispatch, independent of the user interface.
pub mod agent;
pub mod config;
mod llm;
pub mod phase;
pub mod tool_registry;
pub use llm::{ModelResponse, stream_response, stream_response_with_effort};
pub use phase::Checkpoint;

pub use agent::{AgentEvent, AgentOutcome, run_agent};
pub use config::AgentConfig;
pub use tool_registry::ToolRegistry;
pub use tools::{AgentTool, agent_tool};
