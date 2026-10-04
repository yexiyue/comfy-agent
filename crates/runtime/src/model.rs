pub use agent::Checkpoint;
use anyhow::{Result, ensure};
use genai::chat::ChatRequest;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Durable business state, independent of queue and subscription state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatus {
    Queued,
    Running,
    Pausing,
    Paused,
    NeedsAttention,
    Finished,
    StepLimit,
    Failed,
    Cancelled,
    Superseded,
}

impl RunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Finished | Self::StepLimit | Self::Failed | Self::Cancelled | Self::Superseded
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UiMessage {
    pub id: String,
    pub role: String,
    pub parts: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversation {
    pub id: String,
    pub revision: i64,
    pub messages: Vec<UiMessage>,
    pub history: ChatRequest,
    pub active_run_id: Option<String>,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: String,
    pub conversation_id: String,
    pub assistant_id: String,
    pub status: RunStatus,
    pub version: i64,
    pub generation: i64,
    pub dispatch: i64,
    pub attempt_id: Option<String>,
    pub checkpoint: Checkpoint,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    pub tool_schema_hash: String,
    pub supersedes: Option<String>,
    pub recoveries: usize,
    #[serde(default)]
    pub evaluation: bool,
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    pub id: String,
    pub run_id: String,
    pub generation: i64,
    pub outcome: Option<String>,
    pub trace_id: Option<String>,
    pub model_calls: usize,
    pub tool_calls: usize,
    pub known_tokens: u64,
    pub usage_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub sequence: i64,
    pub attempt_id: String,
    pub step: usize,
    pub draft: bool,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExecution {
    pub call_id: String,
    pub step: usize,
    pub name: String,
    pub arguments: Value,
    pub recovery: String,
    pub operation_key: String,
    pub external_id: Option<String>,
    pub output: Option<Value>,
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dispatch {
    pub run_id: String,
    pub dispatch: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandReceipt {
    pub scope: String,
    pub request_id: String,
    pub digest: String,
    pub result: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outbox {
    pub task: Dispatch,
    pub published: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submit {
    pub conversation_id: String,
    pub expected_revision: i64,
    pub request_id: String,
    pub message: UiMessage,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    pub max_steps: usize,
    pub tool_schema_hash: String,
    #[serde(default)]
    pub evaluation: bool,
    pub traceparent: Option<String>,
    pub tracestate: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ControlAction {
    Pause,
    Resume,
    Cancel,
    Steer {
        message: UiMessage,
        expected_revision: i64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Control {
    pub run_id: String,
    pub expected_version: i64,
    pub request_id: String,
    pub action: ControlAction,
}

pub fn user_text(message: &UiMessage) -> Result<String> {
    ensure!(
        !message.id.trim().is_empty() && message.role == "user" && !message.parts.is_empty(),
        "a new user text message is required"
    );
    let mut text = String::new();
    for part in &message.parts {
        ensure!(
            part.get("type").and_then(Value::as_str) == Some("text"),
            "only text inputs are supported"
        );
        text.push_str(
            part.get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("text part requires text"))?,
        );
    }
    ensure!(!text.trim().is_empty(), "message text must not be empty");
    Ok(text)
}
