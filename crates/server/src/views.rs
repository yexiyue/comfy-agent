//! Shared HTTP and stream projections of execution state.
use crate::api::*;
use runtime::model::{Attempt, Conversation, Run, RunStatus, UiMessage};

impl From<UiMessage> for UiMessageView {
    fn from(message: UiMessage) -> Self {
        Self {
            id: message.id,
            role: message.role,
            parts: message.parts,
            metadata: message.metadata,
        }
    }
}
impl From<Conversation> for ConversationView {
    fn from(conversation: Conversation) -> Self {
        Self {
            id: conversation.id,
            revision: conversation.revision,
            messages: conversation.messages.into_iter().map(Into::into).collect(),
            active_run_id: conversation.active_run_id,
            parent_id: conversation.parent_id,
        }
    }
}
impl From<RunStatus> for RunStatusView {
    fn from(status: RunStatus) -> Self {
        match status {
            RunStatus::Queued => Self::Queued,
            RunStatus::Running => Self::Running,
            RunStatus::Pausing => Self::Pausing,
            RunStatus::Paused => Self::Paused,
            RunStatus::NeedsAttention => Self::NeedsAttention,
            RunStatus::Finished => Self::Finished,
            RunStatus::StepLimit => Self::StepLimit,
            RunStatus::Failed => Self::Failed,
            RunStatus::Cancelled => Self::Cancelled,
            RunStatus::Superseded => Self::Superseded,
        }
    }
}
pub(crate) fn public_run(run: &Run) -> RunView {
    RunView {
        id: run.id.clone(),
        conversation_id: run.conversation_id.clone(),
        assistant_id: run.assistant_id.clone(),
        status: run.status.into(),
        version: run.version,
        generation: run.generation,
        attempt_id: run.attempt_id.clone(),
        steps: run.checkpoint.step,
        error: run.error.clone(),
        supersedes: run.supersedes.clone(),
    }
}
impl From<Attempt> for AttemptView {
    fn from(attempt: Attempt) -> Self {
        Self {
            id: attempt.id,
            run_id: attempt.run_id,
            generation: attempt.generation,
            outcome: attempt.outcome,
            trace_id: attempt.trace_id,
            model_calls: attempt.model_calls,
            tool_calls: attempt.tool_calls,
            known_tokens: attempt.known_tokens,
            usage_complete: attempt.usage_complete,
        }
    }
}
