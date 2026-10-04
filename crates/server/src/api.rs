//! Public HTTP contracts; storage and model SDK types stay behind these projections.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Low,
    High,
    Max,
}
impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::High => "high",
            Self::Max => "max",
        }
    }
}
#[derive(Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    pub id: String,
    pub reasoning_efforts: Vec<ReasoningEffort>,
    pub default_reasoning_effort: Option<ReasoningEffort>,
}
#[derive(Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChatConfig {
    pub default_model: String,
    pub models: Vec<ModelOption>,
}

#[derive(Serialize, ToSchema)]
pub struct ApiError {
    pub error: String,
}
#[derive(Serialize, ToSchema)]
pub struct Health {
    pub status: String,
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

/// AI SDK UIMessage envelope. Parts follow UI Message Stream v1.
#[derive(Serialize, ToSchema)]
pub struct UiMessageView {
    pub id: String,
    #[schema(value_type = MessageRole)]
    pub role: String,
    pub parts: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationView {
    pub id: String,
    pub revision: i64,
    pub messages: Vec<UiMessageView>,
    pub active_run_id: Option<String>,
    pub parent_id: Option<String>,
}
#[derive(Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RunStatusView {
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
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub id: String,
    pub conversation_id: String,
    pub assistant_id: String,
    pub status: RunStatusView,
    pub version: i64,
    pub generation: i64,
    pub attempt_id: Option<String>,
    pub steps: usize,
    pub error: Option<String>,
    pub supersedes: Option<String>,
}
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RunStatistics {
    pub model_calls: usize,
    pub tool_calls: usize,
    pub known_tokens: u64,
    pub usage_complete: bool,
}
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttemptView {
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
#[derive(Serialize, ToSchema)]
pub struct RunDetail {
    #[serde(flatten)]
    pub run: RunView,
    pub statistics: RunStatistics,
    pub attempts: Vec<AttemptView>,
}
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub run_id: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RunAction {
    Pause,
    Resume,
    Cancel,
    Steer,
}
