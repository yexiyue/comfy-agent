//! Row codecs and SQL primitives used inside semantic transactions.
use crate::migration::text_column;
use runtime::{
    model::{
        Checkpoint, Conversation, ProgressEvent, Run, RunStatus, Submit, ToolExecution, UiMessage,
    },
    store::{StoreError, StoreResult},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use toasty::{Executor, sql};
use uuid::Uuid;
pub(super) fn infra(error: impl Into<anyhow::Error>) -> StoreError {
    StoreError::Unavailable(error.into())
}
pub(super) fn decode<T: DeserializeOwned>(rows: &[toasty::stmt::Value]) -> StoreResult<Option<T>> {
    rows.first()
        .map(|row| serde_json::from_str(text_column(row, 0).map_err(infra)?).map_err(infra))
        .transpose()
}
pub(super) fn encode<T: Serialize>(value: &T) -> StoreResult<String> {
    serde_json::to_string(value).map_err(infra)
}
pub(super) fn digest<T: Serialize>(value: &T) -> StoreResult<String> {
    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(object) => {
                let mut entries: Vec<_> = object.into_iter().collect();
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(key, value)| (key, canonical(value)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
            value => value,
        }
    }
    let canonical = canonical(serde_json::to_value(value).map_err(infra)?);
    Ok(format!(
        "{:x}",
        Sha256::digest(encode(&canonical)?.as_bytes())
    ))
}
pub(super) async fn load_conversation(
    tx: &mut dyn Executor,
    id: &str,
    lock: bool,
) -> StoreResult<Conversation> {
    let query = if lock {
        "SELECT body FROM agent_conversations WHERE id=$1 FOR UPDATE"
    } else {
        "SELECT body FROM agent_conversations WHERE id=$1"
    };
    let rows = sql::query(query).bind(id).exec(tx).await.map_err(infra)?;
    decode(&rows)?.ok_or(StoreError::NotFound)
}
pub(super) async fn load_run(tx: &mut dyn Executor, id: &str, lock: bool) -> StoreResult<Run> {
    let query = if lock {
        "SELECT body FROM agent_runs WHERE id=$1 FOR UPDATE"
    } else {
        "SELECT body FROM agent_runs WHERE id=$1"
    };
    let rows = sql::query(query).bind(id).exec(tx).await.map_err(infra)?;
    let run: Run = decode(&rows)?.ok_or(StoreError::NotFound)?;
    Checkpoint::decode(serde_json::to_value(&run.checkpoint).map_err(infra)?).map_err(infra)?;
    Ok(run)
}
pub(super) async fn save_conversation(
    tx: &mut dyn Executor,
    conversation: &Conversation,
) -> StoreResult<()> {
    sql::statement("UPDATE agent_conversations SET revision=$2, active_run_id=NULLIF($3,''), body=$4, updated_at=NOW() WHERE id=$1")
        .bind(&conversation.id).bind(conversation.revision).bind(conversation.active_run_id.as_deref().unwrap_or("")).bind(encode(conversation)?)
        .exec(tx).await.map_err(infra)?;
    Ok(())
}
pub(super) async fn save_run(tx: &mut dyn Executor, run: &Run) -> StoreResult<()> {
    let status = serde_json::to_value(run.status).map_err(infra)?;
    sql::statement(
        "UPDATE agent_runs SET status=$2,version=$3,generation=$4,dispatch=$5,body=$6 WHERE id=$1",
    )
    .bind(&run.id)
    .bind(status.as_str().unwrap())
    .bind(run.version)
    .bind(run.generation)
    .bind(run.dispatch)
    .bind(encode(run)?)
    .exec(tx)
    .await
    .map_err(infra)?;
    Ok(())
}
pub(super) async fn insert_run(tx: &mut dyn Executor, run: &Run) -> StoreResult<()> {
    sql::statement("INSERT INTO agent_runs (id,conversation_id,status,version,generation,dispatch,body) VALUES ($1,$2,'queued',$3,$4,$5,$6)")
        .bind(&run.id).bind(&run.conversation_id).bind(run.version).bind(run.generation).bind(run.dispatch).bind(encode(run)?)
        .exec(tx).await.map_err(infra)?;
    schedule(tx, run).await
}
pub(super) async fn schedule(tx: &mut dyn Executor, run: &Run) -> StoreResult<()> {
    sql::statement(
        "INSERT INTO agent_outbox (run_id,dispatch) VALUES ($1,$2) ON CONFLICT DO NOTHING",
    )
    .bind(&run.id)
    .bind(run.dispatch)
    .exec(tx)
    .await
    .map_err(infra)?;
    Ok(())
}
pub(super) async fn message(
    tx: &mut dyn Executor,
    conversation: &mut Conversation,
    message: UiMessage,
    run_id: Option<&str>,
) -> StoreResult<()> {
    if conversation
        .messages
        .iter()
        .any(|existing| existing.id == message.id)
    {
        return Err(StoreError::Invalid("duplicate message id".into()));
    }
    sql::statement("INSERT INTO agent_messages (conversation_id,id,position,run_id,body) VALUES ($1,$2,$3,NULLIF($4,''),$5)")
        .bind(&conversation.id).bind(&message.id).bind(conversation.messages.len() as i64).bind(run_id.unwrap_or("")).bind(encode(&message)?)
        .exec(tx).await.map_err(infra)?;
    conversation.messages.push(message);
    conversation.revision += 1;
    Ok(())
}
pub(super) async fn receipt(
    tx: &mut dyn Executor,
    scope: &str,
    key: &str,
    hash: &str,
) -> StoreResult<Option<String>> {
    if key.trim().is_empty() {
        return Err(StoreError::Invalid("requestId must not be empty".into()));
    }
    let rows =
        sql::query("SELECT digest,result FROM agent_commands WHERE scope=$1 AND request_id=$2")
            .bind(scope)
            .bind(key)
            .exec(tx)
            .await
            .map_err(infra)?;
    if let Some(row) = rows.first() {
        if text_column(row, 0).map_err(infra)? != hash {
            return Err(StoreError::Conflict);
        }
        let value: Value =
            serde_json::from_str(text_column(row, 1).map_err(infra)?).map_err(infra)?;
        return Ok(Some(
            value["runId"]
                .as_str()
                .ok_or_else(|| infra(anyhow::anyhow!("invalid command receipt")))?
                .to_owned(),
        ));
    }
    Ok(None)
}
pub(super) async fn remember(
    tx: &mut dyn Executor,
    scope: &str,
    key: &str,
    hash: &str,
    run_id: &str,
) -> StoreResult<()> {
    sql::statement("INSERT INTO agent_commands VALUES ($1,$2,$3,$4)")
        .bind(scope)
        .bind(key)
        .bind(hash)
        .bind(json!({"runId":run_id}).to_string())
        .exec(tx)
        .await
        .map_err(infra)?;
    Ok(())
}
pub(super) fn make_run(
    conversation: &Conversation,
    command: &Submit,
    supersedes: Option<String>,
) -> StoreResult<Run> {
    Ok(Run {
        id: Uuid::new_v4().to_string(),
        conversation_id: conversation.id.clone(),
        assistant_id: Uuid::new_v4().to_string(),
        status: RunStatus::Queued,
        version: 0,
        generation: 0,
        dispatch: 0,
        attempt_id: None,
        checkpoint: Checkpoint::new(conversation.history.clone(), command.max_steps)
            .map_err(|error| StoreError::Invalid(error.to_string()))?,
        model: command.model.clone(),
        tool_schema_hash: command.tool_schema_hash.clone(),
        supersedes,
        recoveries: 0,
        evaluation: command.evaluation,
        traceparent: command.traceparent.clone(),
        tracestate: command.tracestate.clone(),
        error: None,
    })
}

pub(super) async fn unsafe_pending_tool(tx: &mut dyn Executor, run: &Run) -> StoreResult<bool> {
    if !run.checkpoint.tool_inflight {
        return Ok(false);
    }
    let Some(tool) = pending_tool(tx, run).await? else {
        return Ok(true);
    };
    Ok(match tool.recovery.as_str() {
        "safe-to-retry" | "idempotent" => false,
        "reconcilable" => tool.external_id.is_none(),
        _ => true,
    })
}

/// Resuming with a key or external ID is safe; abandoning that operation is not.
pub(super) async fn unresolved_external_operation(
    tx: &mut dyn Executor,
    run: &Run,
) -> StoreResult<bool> {
    if !run.checkpoint.tool_inflight {
        return Ok(false);
    }
    Ok(pending_tool(tx, run)
        .await?
        .is_none_or(|tool| tool.recovery != "safe-to-retry"))
}

async fn pending_tool(tx: &mut dyn Executor, run: &Run) -> StoreResult<Option<ToolExecution>> {
    let Some(decision) = &run.checkpoint.decision else {
        return Ok(None);
    };
    let calls = decision.tool_calls();
    let Some(call) = calls.get(run.checkpoint.next_tool) else {
        return Ok(None);
    };
    let rows = sql::query("SELECT body FROM agent_tool_executions WHERE run_id=$1 AND call_id=$2")
        .bind(&run.id)
        .bind(&call.call_id)
        .exec(tx)
        .await
        .map_err(infra)?;
    decode(&rows)
}

pub(super) async fn valid_events(
    tx: &mut dyn Executor,
    id: &str,
    after: i64,
    limit: i64,
) -> StoreResult<Vec<ProgressEvent>> {
    let rows=sql::query("SELECT body,draft::text FROM agent_events WHERE run_id=$1 AND sequence>$2 AND valid=TRUE ORDER BY sequence LIMIT $3").bind(id).bind(after).bind(limit).exec(tx).await.map_err(infra)?;
    rows.iter()
        .map(|row| {
            let mut event: ProgressEvent =
                serde_json::from_str(text_column(row, 0).map_err(infra)?).map_err(infra)?;
            event.draft = text_column(row, 1).map_err(infra)? == "true";
            Ok(event)
        })
        .collect()
}

pub(super) async fn project_assistant(
    tx: &mut dyn Executor,
    conversation: &mut Conversation,
    run: &Run,
) -> StoreResult<()> {
    let events = valid_events(tx, &run.id, 0, i64::MAX).await?;
    if let Some(value) = runtime::projection::assistant(run, &events) {
        if let Some(index) = conversation
            .messages
            .iter()
            .position(|message| message.id == value.id)
        {
            if encode(&conversation.messages[index])? != encode(&value)? {
                sql::statement(
                    "UPDATE agent_messages SET body=$3 WHERE conversation_id=$1 AND id=$2",
                )
                .bind(&conversation.id)
                .bind(&value.id)
                .bind(encode(&value)?)
                .exec(tx)
                .await
                .map_err(infra)?;
                conversation.messages[index] = value;
                conversation.revision += 1;
            }
        } else {
            message(tx, conversation, value, Some(&run.id)).await?;
        }
    }
    Ok(())
}
