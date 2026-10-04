//! Semantic operations define transaction boundaries for infrastructure.

use futures::future::BoxFuture;
use serde_json::Value;

use crate::model::{
    Attempt, Control, Conversation, Dispatch, ProgressEvent, Run, Submit, ToolExecution,
};

#[derive(Debug)]
pub enum StoreError {
    NotFound,
    Conflict,
    Invalid(String),
    Unavailable(anyhow::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(formatter, "not found"),
            Self::Conflict => write!(formatter, "state or version conflict"),
            Self::Invalid(message) => write!(formatter, "{message}"),
            Self::Unavailable(_) => write!(formatter, "storage unavailable"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<anyhow::Error> for StoreError {
    fn from(error: anyhow::Error) -> Self {
        Self::Unavailable(error)
    }
}

pub type StoreResult<T> = Result<T, StoreError>;

/// Read models return a consistent database snapshot, not mutable ORM records.
pub trait ConversationStore: Send + Sync {
    fn conversation<'a>(&'a self, id: &'a str) -> BoxFuture<'a, StoreResult<Conversation>>;
    fn run<'a>(&'a self, id: &'a str) -> BoxFuture<'a, StoreResult<Run>>;
    fn run_with_attempts<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, StoreResult<(Run, Vec<Attempt>)>>;
    fn command_result<'a>(
        &'a self,
        scope: &'a str,
        request_id: &'a str,
    ) -> BoxFuture<'a, StoreResult<Option<Value>>>;
    fn create(&self, conversation: Conversation) -> BoxFuture<'_, StoreResult<Conversation>>;
    fn list(&self, offset: i64, limit: i64) -> BoxFuture<'_, StoreResult<Vec<Conversation>>>;
    fn submit(&self, command: Submit) -> BoxFuture<'_, StoreResult<Run>>;
    fn control(&self, command: Control) -> BoxFuture<'_, StoreResult<Run>>;
    fn claim(&self, task: Dispatch, lease_seconds: i64) -> BoxFuture<'_, StoreResult<Option<Run>>>;
    fn heartbeat<'a>(
        &'a self,
        run_id: &'a str,
        generation: i64,
        lease_seconds: i64,
    ) -> BoxFuture<'a, StoreResult<bool>>;
    fn commit(
        &self,
        run: Run,
        attempt: Attempt,
        events: Vec<ProgressEvent>,
        tools: Vec<ToolExecution>,
        max_event_bytes: i64,
    ) -> BoxFuture<'_, StoreResult<Run>>;
    fn append<'a>(
        &'a self,
        run_id: &'a str,
        generation: i64,
        events: Vec<ProgressEvent>,
        max_event_bytes: i64,
    ) -> BoxFuture<'a, StoreResult<()>>;
    fn events<'a>(
        &'a self,
        run_id: &'a str,
        after: i64,
        limit: i64,
    ) -> BoxFuture<'a, StoreResult<Vec<ProgressEvent>>>;
    fn progress<'a>(
        &'a self,
        run_id: &'a str,
        after: i64,
        limit: i64,
    ) -> BoxFuture<'a, StoreResult<(Run, Vec<ProgressEvent>)>>;
    fn outbox(&self, limit: i64) -> BoxFuture<'_, StoreResult<Vec<Dispatch>>>;
    fn published(&self, task: Dispatch) -> BoxFuture<'_, StoreResult<()>>;
    fn attempt<'a>(&'a self, id: &'a str) -> BoxFuture<'a, StoreResult<Attempt>>;
    fn attempts<'a>(&'a self, run_id: &'a str) -> BoxFuture<'a, StoreResult<Vec<Attempt>>>;
    fn tool<'a>(
        &'a self,
        run_id: &'a str,
        call_id: &'a str,
    ) -> BoxFuture<'a, StoreResult<Option<ToolExecution>>>;
    fn record_external<'a>(
        &'a self,
        run_id: &'a str,
        generation: i64,
        call_id: &'a str,
        external_id: String,
    ) -> BoxFuture<'a, StoreResult<()>>;
    fn settle<'a>(
        &'a self,
        run_id: &'a str,
        attempt_id: &'a str,
        generation: i64,
        reason: &'a str,
    ) -> BoxFuture<'a, StoreResult<()>>;
    fn recover(&self, max_recoveries: usize) -> BoxFuture<'_, StoreResult<usize>>;
}
