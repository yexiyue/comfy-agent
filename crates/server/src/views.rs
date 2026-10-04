//! Shared HTTP and stream projections of execution state.
use runtime::model::Run;
use serde_json::{Value, json};
pub(crate) fn public_run(run: &Run) -> Value {
    json!({
        "id": run.id,
        "conversationId": run.conversation_id,
        "assistantId": run.assistant_id,
        "status": run.status,
        "version": run.version,
        "generation": run.generation,
        "attemptId": run.attempt_id,
        "steps": run.checkpoint.step,
        "error": run.error,
        "supersedes": run.supersedes,
    })
}
