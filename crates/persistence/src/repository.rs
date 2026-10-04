//! Semantic units of work on one Toasty transaction connection.

use anyhow::Result;
use futures::future::BoxFuture;
use genai::chat::ChatMessage;
use runtime::{
    model::*,
    store::{ConversationStore, StoreError, StoreResult},
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use toasty::{Db, Executor, sql};
use uuid::Uuid;

use crate::migration::{self, text_column};

#[derive(Clone)]
pub struct PostgresStore {
    db: Db,
}

fn infra(error: impl Into<anyhow::Error>) -> StoreError {
    StoreError::Unavailable(error.into())
}
fn decode<T: DeserializeOwned>(rows: &[toasty::stmt::Value]) -> StoreResult<Option<T>> {
    rows.first()
        .map(|row| serde_json::from_str(text_column(row, 0).map_err(infra)?).map_err(infra))
        .transpose()
}
fn encode<T: Serialize>(value: &T) -> StoreResult<String> {
    serde_json::to_string(value).map_err(infra)
}
fn digest<T: Serialize>(value: &T) -> StoreResult<String> {
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
async fn load_conversation(
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
async fn load_run(tx: &mut dyn Executor, id: &str, lock: bool) -> StoreResult<Run> {
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
async fn save_conversation(tx: &mut dyn Executor, conversation: &Conversation) -> StoreResult<()> {
    sql::statement("UPDATE agent_conversations SET revision=$2, active_run_id=NULLIF($3,''), body=$4, updated_at=NOW() WHERE id=$1")
        .bind(&conversation.id).bind(conversation.revision).bind(conversation.active_run_id.as_deref().unwrap_or("")).bind(encode(conversation)?)
        .exec(tx).await.map_err(infra)?;
    Ok(())
}
async fn save_run(tx: &mut dyn Executor, run: &Run) -> StoreResult<()> {
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
async fn insert_run(tx: &mut dyn Executor, run: &Run) -> StoreResult<()> {
    sql::statement("INSERT INTO agent_runs (id,conversation_id,status,version,generation,dispatch,body) VALUES ($1,$2,'queued',$3,$4,$5,$6)")
        .bind(&run.id).bind(&run.conversation_id).bind(run.version).bind(run.generation).bind(run.dispatch).bind(encode(run)?)
        .exec(tx).await.map_err(infra)?;
    schedule(tx, run).await
}
async fn schedule(tx: &mut dyn Executor, run: &Run) -> StoreResult<()> {
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
async fn message(
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
async fn receipt(
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
async fn remember(
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
fn make_run(
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

impl PostgresStore {
    pub async fn open(url: &str) -> Result<Self> {
        let mut db = crate::connect(url).await?;
        migration::check(&mut db).await?;
        Ok(Self { db })
    }

    async fn guarded(tx: &mut dyn Executor, id: &str, generation: i64) -> StoreResult<Run> {
        let rows = sql::query("SELECT body FROM agent_runs WHERE id=$1 AND generation=$2 AND status='running' AND lease_until>NOW() FOR UPDATE")
            .bind(id).bind(generation).exec(tx).await.map_err(infra)?;
        decode(&rows)?.ok_or(StoreError::Conflict)
    }

    async fn write_events(
        tx: &mut dyn Executor,
        id: &str,
        events: &[ProgressEvent],
        max_bytes: i64,
    ) -> StoreResult<()> {
        for event in events {
            let body = encode(event)?;
            let rows = sql::query("UPDATE agent_runs SET next_sequence=next_sequence+1,event_bytes=event_bytes+$2 WHERE id=$1 AND event_bytes+$2<=$3 RETURNING next_sequence::text")
                .bind(id).bind(body.len() as i64).bind(max_bytes).exec(tx).await.map_err(infra)?;
            let Some(row) = rows.first() else {
                return Err(StoreError::Invalid("progress capacity exceeded".into()));
            };
            let sequence: i64 = text_column(row, 0).map_err(infra)?.parse().map_err(infra)?;
            let mut saved = event.clone();
            saved.sequence = sequence;
            sql::statement("INSERT INTO agent_events (run_id,sequence,attempt_id,step,draft,body) VALUES ($1,$2,$3,$4,$5,$6)")
                .bind(id).bind(sequence).bind(&event.attempt_id).bind(event.step as i64).bind(event.draft).bind(encode(&saved)?)
                .exec(tx).await.map_err(infra)?;
        }
        Ok(())
    }
}

impl ConversationStore for PostgresStore {
    fn conversation<'a>(&'a self, id: &'a str) -> BoxFuture<'a, StoreResult<Conversation>> {
        Box::pin(async move {
            let mut db = self.db.clone();
            let mut tx = db.transaction().await.map_err(infra)?;
            let mut conversation = load_conversation(&mut tx, id, true).await?;
            if let Some(id) = &conversation.active_run_id {
                let run = load_run(&mut tx, id, false).await?;
                let events = valid_events(&mut tx, id, 0, i64::MAX).await?;
                if let Some(message) = runtime::projection::assistant(&run, &events) {
                    if let Some(index) = conversation
                        .messages
                        .iter()
                        .position(|existing| existing.id == message.id)
                    {
                        conversation.messages[index] = message;
                    } else {
                        conversation.messages.push(message);
                    }
                }
            }
            tx.commit().await.map_err(infra)?;
            Ok(conversation)
        })
    }
    fn run<'a>(&'a self, id: &'a str) -> BoxFuture<'a, StoreResult<Run>> {
        Box::pin(async move { load_run(&mut self.db.clone(), id, false).await })
    }
    fn run_with_attempts<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, StoreResult<(Run, Vec<Attempt>)>> {
        Box::pin(async move {
            let original = self.run(id).await?;
            let mut db = self.db.clone();
            let mut tx = db.transaction().await.map_err(infra)?;
            load_conversation(&mut tx, &original.conversation_id, true).await?;
            let run = load_run(&mut tx, id, true).await?;
            let rows =
                sql::query("SELECT body FROM agent_attempts WHERE run_id=$1 ORDER BY generation")
                    .bind(id)
                    .exec(&mut tx)
                    .await
                    .map_err(infra)?;
            let attempts = rows
                .iter()
                .map(|row| decode(std::slice::from_ref(row))?.ok_or(StoreError::NotFound))
                .collect::<StoreResult<Vec<_>>>()?;
            tx.commit().await.map_err(infra)?;
            Ok((run, attempts))
        })
    }
    fn command_result<'a>(
        &'a self,
        scope: &'a str,
        request_id: &'a str,
    ) -> BoxFuture<'a, StoreResult<Option<Value>>> {
        Box::pin(async move {
            let rows =
                sql::query("SELECT result FROM agent_commands WHERE scope=$1 AND request_id=$2")
                    .bind(scope)
                    .bind(request_id)
                    .exec(&mut self.db.clone())
                    .await
                    .map_err(infra)?;
            decode(&rows)
        })
    }
    fn create(&self, conversation: Conversation) -> BoxFuture<'_, StoreResult<Conversation>> {
        Box::pin(async move {
            let mut db = self.db.clone();
            let mut tx = db.transaction().await.map_err(infra)?;
            if conversation.active_run_id.is_some() || conversation.revision != 0 {
                return Err(StoreError::Invalid("invalid new conversation".into()));
            }
            let mut value = conversation.clone();
            value.messages.clear();
            sql::statement("INSERT INTO agent_conversations (id,revision,body) VALUES ($1,0,$2)")
                .bind(&value.id)
                .bind(encode(&value)?)
                .exec(&mut tx)
                .await
                .map_err(infra)?;
            for initial in conversation.messages {
                message(&mut tx, &mut value, initial, None).await?;
            }
            save_conversation(&mut tx, &value).await?;
            tx.commit().await.map_err(infra)?;
            Ok(value)
        })
    }
    fn list(&self, offset: i64, limit: i64) -> BoxFuture<'_, StoreResult<Vec<Conversation>>> {
        Box::pin(async move {
            if offset < 0 || !(1..=100).contains(&limit) {
                return Err(StoreError::Invalid("invalid pagination".into()));
            }
            let rows=sql::query("SELECT body FROM agent_conversations ORDER BY updated_at DESC,id LIMIT $1 OFFSET $2").bind(limit).bind(offset).exec(&mut self.db.clone()).await.map_err(infra)?;
            rows.iter()
                .map(|row| decode(std::slice::from_ref(row))?.ok_or(StoreError::NotFound))
                .collect()
        })
    }
    fn submit(&self, command: Submit) -> BoxFuture<'_, StoreResult<Run>> {
        Box::pin(async move {
            let text =
                user_text(&command.message).map_err(|e| StoreError::Invalid(e.to_string()))?;
            let hash = digest(
                &json!({"conversationId":command.conversation_id,"expectedRevision":command.expected_revision,"requestId":command.request_id,"message":command.message}),
            )?;
            let mut db = self.db.clone();
            let mut tx = db.transaction().await.map_err(infra)?;
            let mut conversation =
                load_conversation(&mut tx, &command.conversation_id, true).await?;
            if let Some(id) = receipt(&mut tx, &conversation.id, &command.request_id, &hash).await?
            {
                return load_run(&mut tx, &id, false).await;
            }
            if conversation.revision != command.expected_revision
                || conversation.active_run_id.is_some()
            {
                return Err(StoreError::Conflict);
            }
            message(&mut tx, &mut conversation, command.message.clone(), None).await?;
            conversation.history.messages.push(ChatMessage::user(text));
            let run = make_run(&conversation, &command, None)?;
            insert_run(&mut tx, &run).await?;
            conversation.active_run_id = Some(run.id.clone());
            save_conversation(&mut tx, &conversation).await?;
            remember(
                &mut tx,
                &conversation.id,
                &command.request_id,
                &hash,
                &run.id,
            )
            .await?;
            tx.commit().await.map_err(infra)?;
            Ok(run)
        })
    }
    fn control(&self, command: Control) -> BoxFuture<'_, StoreResult<Run>> {
        Box::pin(async move {
            let original = self.run(&command.run_id).await?;
            let hash = digest(&command)?;
            let mut db = self.db.clone();
            let mut tx = db.transaction().await.map_err(infra)?;
            let mut conversation =
                load_conversation(&mut tx, &original.conversation_id, true).await?;
            if let Some(id) = receipt(&mut tx, &conversation.id, &command.request_id, &hash).await?
            {
                return load_run(&mut tx, &id, false).await;
            }
            let mut run = load_run(&mut tx, &command.run_id, true).await?;
            if run.version != command.expected_version
                || run.status.is_terminal()
                || conversation.active_run_id.as_deref() != Some(&run.id)
            {
                return Err(StoreError::Conflict);
            }
            run.version += 1;
            run.generation += 1;
            match &command.action {
                ControlAction::Pause => {
                    run.status = match run.status {
                        RunStatus::Queued => RunStatus::Paused,
                        RunStatus::Running => RunStatus::Pausing,
                        _ => return Err(StoreError::Conflict),
                    };
                }
                ControlAction::Resume => {
                    if run.status != RunStatus::Paused {
                        return Err(StoreError::Conflict);
                    }
                    run.status = RunStatus::Queued;
                    run.dispatch += 1;
                    sql::statement(
                        "UPDATE agent_events SET valid=FALSE WHERE run_id=$1 AND draft=TRUE",
                    )
                    .bind(&run.id)
                    .exec(&mut tx)
                    .await
                    .map_err(infra)?;
                    schedule(&mut tx, &run).await?;
                }
                ControlAction::Cancel => {
                    run.status = RunStatus::Cancelled;
                    conversation.active_run_id = None;
                    conversation.history = run.checkpoint.superseded_history();
                    conversation.revision += 1;
                }
                ControlAction::Steer {
                    message: new_message,
                    expected_revision,
                } => {
                    if run.status != RunStatus::Paused
                        || conversation.revision != *expected_revision
                    {
                        return Err(StoreError::Conflict);
                    }
                    let text =
                        user_text(new_message).map_err(|e| StoreError::Invalid(e.to_string()))?;
                    conversation.history = run.checkpoint.superseded_history();
                    conversation.history.messages.push(ChatMessage::user(text));
                    run.status = RunStatus::Superseded;
                    project_assistant(&mut tx, &mut conversation, &run).await?;
                    message(&mut tx, &mut conversation, new_message.clone(), None).await?;
                    save_run(&mut tx, &run).await?;
                    let next_command = Submit {
                        conversation_id: conversation.id.clone(),
                        expected_revision: conversation.revision,
                        request_id: command.request_id.clone(),
                        message: new_message.clone(),
                        model: run.model.clone(),
                        max_steps: run.checkpoint.max_steps,
                        tool_schema_hash: run.tool_schema_hash.clone(),
                        evaluation: run.evaluation,
                        traceparent: run.traceparent.clone(),
                        tracestate: run.tracestate.clone(),
                    };
                    let next = make_run(&conversation, &next_command, Some(run.id.clone()))?;
                    insert_run(&mut tx, &next).await?;
                    conversation.active_run_id = Some(next.id.clone());
                    save_conversation(&mut tx, &conversation).await?;
                    remember(
                        &mut tx,
                        &conversation.id,
                        &command.request_id,
                        &hash,
                        &next.id,
                    )
                    .await?;
                    tx.commit().await.map_err(infra)?;
                    return Ok(next);
                }
            }
            project_assistant(&mut tx, &mut conversation, &run).await?;
            save_run(&mut tx, &run).await?;
            save_conversation(&mut tx, &conversation).await?;
            remember(
                &mut tx,
                &conversation.id,
                &command.request_id,
                &hash,
                &run.id,
            )
            .await?;
            tx.commit().await.map_err(infra)?;
            Ok(run)
        })
    }
    fn claim(&self, task: Dispatch, lease_seconds: i64) -> BoxFuture<'_, StoreResult<Option<Run>>> {
        Box::pin(async move {
            if lease_seconds <= 0 {
                return Err(StoreError::Invalid(
                    "lease duration must be positive".into(),
                ));
            }
            let original = self.run(&task.run_id).await?;
            let mut db = self.db.clone();
            let mut tx = db.transaction().await.map_err(infra)?;
            // Status changes can acquire foreign-key locks on the conversation as well.
            // Claim must use the same conversation -> run order as readers and controls.
            load_conversation(&mut tx, &original.conversation_id, true).await?;
            let rows=sql::query("SELECT body FROM agent_runs WHERE id=$1 AND dispatch=$2 AND status='queued' FOR UPDATE").bind(&task.run_id).bind(task.dispatch).exec(&mut tx).await.map_err(infra)?;
            let Some(mut run): Option<Run> = decode(&rows)? else {
                return Ok(None);
            };
            run.status = RunStatus::Running;
            run.generation += 1;
            run.version += 1;
            run.attempt_id = Some(Uuid::new_v4().to_string());
            let attempt = Attempt {
                id: run.attempt_id.clone().unwrap(),
                run_id: run.id.clone(),
                generation: run.generation,
                outcome: None,
                trace_id: None,
                model_calls: 0,
                tool_calls: 0,
                known_tokens: 0,
                usage_complete: true,
            };
            save_run(&mut tx, &run).await?;
            sql::statement("UPDATE agent_runs SET lease_until=NOW()+($2::bigint * INTERVAL '1 second') WHERE id=$1").bind(&run.id).bind(lease_seconds).exec(&mut tx).await.map_err(infra)?;
            sql::statement(
                "INSERT INTO agent_attempts (id,run_id,generation,body) VALUES ($1,$2,$3,$4)",
            )
            .bind(&attempt.id)
            .bind(&run.id)
            .bind(run.generation)
            .bind(encode(&attempt)?)
            .exec(&mut tx)
            .await
            .map_err(infra)?;
            tx.commit().await.map_err(infra)?;
            Ok(Some(run))
        })
    }
    fn heartbeat<'a>(
        &'a self,
        id: &'a str,
        generation: i64,
        lease: i64,
    ) -> BoxFuture<'a, StoreResult<bool>> {
        Box::pin(async move {
            let changed=sql::statement("UPDATE agent_runs SET lease_until=NOW()+($3::bigint * INTERVAL '1 second') WHERE id=$1 AND generation=$2 AND status='running' AND lease_until>NOW()")
            .bind(id).bind(generation).bind(lease).exec(&mut self.db.clone()).await.map_err(infra)?;
            Ok(changed == 1)
        })
    }
    fn commit(
        &self,
        mut run: Run,
        attempt: Attempt,
        events: Vec<ProgressEvent>,
        tools: Vec<ToolExecution>,
        max_bytes: i64,
    ) -> BoxFuture<'_, StoreResult<Run>> {
        Box::pin(async move {
            let mut db = self.db.clone();
            let mut tx = db.transaction().await.map_err(infra)?;
            let mut conversation = load_conversation(&mut tx, &run.conversation_id, true).await?;
            let current = Self::guarded(&mut tx, &run.id, run.generation).await?;
            if current.version != run.version
                || current.attempt_id.as_deref() != Some(&attempt.id)
                || attempt.generation != run.generation
            {
                return Err(StoreError::Conflict);
            }
            run.version += 1;
            Self::write_events(&mut tx, &run.id, &events, max_bytes).await?;
            if !run.checkpoint.model_inflight {
                sql::statement("UPDATE agent_events SET draft=FALSE WHERE run_id=$1 AND attempt_id=$2 AND step=$3 AND valid=TRUE").bind(&run.id).bind(&attempt.id).bind(run.checkpoint.step as i64).exec(&mut tx).await.map_err(infra)?;
            }
            for tool in tools {
                sql::statement("INSERT INTO agent_tool_executions VALUES ($1,$2,$3) ON CONFLICT (run_id,call_id) DO UPDATE SET body=EXCLUDED.body").bind(&run.id).bind(&tool.call_id).bind(encode(&tool)?).exec(&mut tx).await.map_err(infra)?;
            }
            sql::statement("UPDATE agent_attempts SET body=$2,ended_at=CASE WHEN $3 THEN NOW() ELSE ended_at END WHERE id=$1").bind(&attempt.id).bind(encode(&attempt)?).bind(attempt.outcome.is_some()).exec(&mut tx).await.map_err(infra)?;
            if run.status.is_terminal() {
                conversation.history = run.checkpoint.history.clone();
                conversation.active_run_id = None;
                conversation.revision += 1;
            }
            project_assistant(&mut tx, &mut conversation, &run).await?;
            save_run(&mut tx, &run).await?;
            save_conversation(&mut tx, &conversation).await?;
            tx.commit().await.map_err(infra)?;
            Ok(run)
        })
    }
    fn append<'a>(
        &'a self,
        id: &'a str,
        generation: i64,
        events: Vec<ProgressEvent>,
        max_bytes: i64,
    ) -> BoxFuture<'a, StoreResult<()>> {
        Box::pin(async move {
            let original = self.run(id).await?;
            let mut db = self.db.clone();
            let mut tx = db.transaction().await.map_err(infra)?;
            load_conversation(&mut tx, &original.conversation_id, true).await?;
            Self::guarded(&mut tx, id, generation).await?;
            Self::write_events(&mut tx, id, &events, max_bytes).await?;
            tx.commit().await.map_err(infra)?;
            Ok(())
        })
    }
    fn events<'a>(
        &'a self,
        id: &'a str,
        after: i64,
        limit: i64,
    ) -> BoxFuture<'a, StoreResult<Vec<ProgressEvent>>> {
        Box::pin(
            async move { valid_events(&mut self.db.clone(), id, after, limit.clamp(1, 256)).await },
        )
    }
    fn outbox(&self, limit: i64) -> BoxFuture<'_, StoreResult<Vec<Dispatch>>> {
        Box::pin(async move {
            let rows=sql::query("SELECT json_build_object('run_id',run_id,'dispatch',dispatch)::text FROM agent_outbox WHERE published=FALSE ORDER BY created_at,run_id LIMIT $1").bind(limit.clamp(1,256)).exec(&mut self.db.clone()).await.map_err(infra)?;
            rows.iter()
                .map(|row| decode(std::slice::from_ref(row))?.ok_or(StoreError::NotFound))
                .collect()
        })
    }
    fn progress<'a>(
        &'a self,
        id: &'a str,
        after: i64,
        limit: i64,
    ) -> BoxFuture<'a, StoreResult<(Run, Vec<ProgressEvent>)>> {
        Box::pin(async move {
            let original = self.run(id).await?;
            let mut db = self.db.clone();
            let mut tx = db.transaction().await.map_err(infra)?;
            load_conversation(&mut tx, &original.conversation_id, true).await?;
            let run = load_run(&mut tx, id, false).await?;
            let events = valid_events(&mut tx, id, after, limit.clamp(1, 256)).await?;
            tx.commit().await.map_err(infra)?;
            Ok((run, events))
        })
    }
    fn published(&self, task: Dispatch) -> BoxFuture<'_, StoreResult<()>> {
        Box::pin(async move {
            sql::statement(
                "UPDATE agent_outbox SET published=TRUE WHERE run_id=$1 AND dispatch=$2",
            )
            .bind(task.run_id)
            .bind(task.dispatch)
            .exec(&mut self.db.clone())
            .await
            .map_err(infra)?;
            Ok(())
        })
    }

    fn attempt<'a>(&'a self, id: &'a str) -> BoxFuture<'a, StoreResult<Attempt>> {
        Box::pin(async move {
            let rows = sql::query("SELECT body FROM agent_attempts WHERE id=$1")
                .bind(id)
                .exec(&mut self.db.clone())
                .await
                .map_err(infra)?;
            decode(&rows)?.ok_or(StoreError::NotFound)
        })
    }
    fn attempts<'a>(&'a self, id: &'a str) -> BoxFuture<'a, StoreResult<Vec<Attempt>>> {
        Box::pin(async move {
            let rows =
                sql::query("SELECT body FROM agent_attempts WHERE run_id=$1 ORDER BY generation")
                    .bind(id)
                    .exec(&mut self.db.clone())
                    .await
                    .map_err(infra)?;
            rows.iter()
                .map(|row| decode(std::slice::from_ref(row))?.ok_or(StoreError::NotFound))
                .collect()
        })
    }
    fn tool<'a>(
        &'a self,
        id: &'a str,
        call_id: &'a str,
    ) -> BoxFuture<'a, StoreResult<Option<ToolExecution>>> {
        Box::pin(async move {
            let rows =
                sql::query("SELECT body FROM agent_tool_executions WHERE run_id=$1 AND call_id=$2")
                    .bind(id)
                    .bind(call_id)
                    .exec(&mut self.db.clone())
                    .await
                    .map_err(infra)?;
            decode(&rows)
        })
    }
    fn record_external<'a>(
        &'a self,
        id: &'a str,
        generation: i64,
        call_id: &'a str,
        external_id: String,
    ) -> BoxFuture<'a, StoreResult<()>> {
        Box::pin(async move {
            if external_id.trim().is_empty() || external_id.len() > 1024 {
                return Err(StoreError::Invalid("invalid external operation ID".into()));
            }
            let original = self.run(id).await?;
            let mut db = self.db.clone();
            let mut tx = db.transaction().await.map_err(infra)?;
            load_conversation(&mut tx, &original.conversation_id, true).await?;
            Self::guarded(&mut tx, id, generation).await?;
            let rows = sql::query(
                "SELECT body FROM agent_tool_executions WHERE run_id=$1 AND call_id=$2 FOR UPDATE",
            )
            .bind(id)
            .bind(call_id)
            .exec(&mut tx)
            .await
            .map_err(infra)?;
            let mut tool: ToolExecution = decode(&rows)?.ok_or(StoreError::NotFound)?;
            if tool
                .external_id
                .as_ref()
                .is_some_and(|existing| existing != &external_id)
            {
                return Err(StoreError::Conflict);
            }
            tool.external_id = Some(external_id);
            sql::statement(
                "UPDATE agent_tool_executions SET body=$3 WHERE run_id=$1 AND call_id=$2",
            )
            .bind(id)
            .bind(call_id)
            .bind(encode(&tool)?)
            .exec(&mut tx)
            .await
            .map_err(infra)?;
            tx.commit().await.map_err(infra)?;
            Ok(())
        })
    }
    fn settle<'a>(
        &'a self,
        id: &'a str,
        attempt_id: &'a str,
        generation: i64,
        reason: &'a str,
    ) -> BoxFuture<'a, StoreResult<()>> {
        Box::pin(async move {
            let original = self.run(id).await?;
            let mut db = self.db.clone();
            let mut tx = db.transaction().await.map_err(infra)?;
            let mut conversation =
                load_conversation(&mut tx, &original.conversation_id, true).await?;
            let mut run = load_run(&mut tx, id, true).await?;
            if run.attempt_id.as_deref() != Some(attempt_id) {
                return Err(StoreError::Conflict);
            }
            let allowed = run.generation == generation
                || (run.generation == generation + 1
                    && matches!(run.status, RunStatus::Pausing | RunStatus::Cancelled));
            if !allowed {
                return Err(StoreError::Conflict);
            }
            if run.status == RunStatus::Running {
                let rows =
                    sql::query("SELECT id FROM agent_runs WHERE id=$1 AND lease_until>NOW()")
                        .bind(id)
                        .exec(&mut tx)
                        .await
                        .map_err(infra)?;
                if rows.is_empty() {
                    return Ok(());
                }
            }
            let unsafe_tool = unsafe_pending_tool(&mut tx, &run).await?;
            if run.status == RunStatus::Pausing {
                run.status = if unsafe_tool {
                    RunStatus::NeedsAttention
                } else {
                    RunStatus::Paused
                };
                if unsafe_tool {
                    run.error = Some("external operation outcome needs reconciliation".into());
                }
            } else if run.status == RunStatus::Running {
                run.generation += 1;
                if unsafe_tool {
                    run.status = RunStatus::NeedsAttention;
                    run.error = Some("external operation outcome needs reconciliation".into());
                } else if reason == "shutdown" {
                    run.status = RunStatus::Queued;
                    run.dispatch += 1;
                    sql::statement(
                        "UPDATE agent_events SET valid=FALSE WHERE run_id=$1 AND draft=TRUE",
                    )
                    .bind(id)
                    .exec(&mut tx)
                    .await
                    .map_err(infra)?;
                    schedule(&mut tx, &run).await?;
                } else {
                    run.status = RunStatus::Failed;
                    run.error = Some("execution failed; see server diagnostics".into());
                }
            }
            let rows = sql::query("SELECT body FROM agent_attempts WHERE id=$1 FOR UPDATE")
                .bind(attempt_id)
                .exec(&mut tx)
                .await
                .map_err(infra)?;
            let mut attempt: Attempt = decode(&rows)?.ok_or(StoreError::NotFound)?;
            if attempt.outcome.is_none() {
                attempt.outcome = Some(
                    match run.status {
                        RunStatus::Paused => "paused",
                        RunStatus::Cancelled => "cancelled",
                        RunStatus::NeedsAttention => "needs-attention",
                        _ => reason,
                    }
                    .to_owned(),
                );
                sql::statement("UPDATE agent_attempts SET body=$2,ended_at=NOW() WHERE id=$1")
                    .bind(attempt_id)
                    .bind(encode(&attempt)?)
                    .exec(&mut tx)
                    .await
                    .map_err(infra)?;
            }
            run.version += 1;
            if run.status.is_terminal() && conversation.active_run_id.as_deref() == Some(id) {
                conversation.active_run_id = None;
                conversation.history = run.checkpoint.superseded_history();
                save_conversation(&mut tx, &conversation).await?;
            }
            project_assistant(&mut tx, &mut conversation, &run).await?;
            save_conversation(&mut tx, &conversation).await?;
            save_run(&mut tx, &run).await?;
            tx.commit().await.map_err(infra)?;
            Ok(())
        })
    }
    fn recover(&self, max_recoveries: usize) -> BoxFuture<'_, StoreResult<usize>> {
        Box::pin(async move {
            let rows=sql::query("SELECT id FROM agent_runs WHERE status IN ('running','pausing') AND lease_until<=NOW() LIMIT 100").exec(&mut self.db.clone()).await.map_err(infra)?;
            let mut count = 0;
            for row in rows {
                let id = text_column(&row, 0).map_err(infra)?;
                let original = self.run(id).await?;
                let mut db = self.db.clone();
                let mut tx = db.transaction().await.map_err(infra)?;
                load_conversation(&mut tx, &original.conversation_id, true).await?;
                let rows=sql::query("SELECT body FROM agent_runs WHERE id=$1 AND status IN ('running','pausing') AND lease_until<=NOW() FOR UPDATE").bind(id).exec(&mut tx).await.map_err(infra)?;
                let Some(mut run): Option<Run> = decode(&rows)? else {
                    continue;
                };
                let unsafe_tool = unsafe_pending_tool(&mut tx, &run).await?;
                let was_pausing = run.status == RunStatus::Pausing;
                if let Some(attempt_id) = &run.attempt_id {
                    let rows = sql::query("SELECT body FROM agent_attempts WHERE id=$1")
                        .bind(attempt_id)
                        .exec(&mut tx)
                        .await
                        .map_err(infra)?;
                    if let Some(mut attempt) = decode::<Attempt>(&rows)? {
                        attempt.outcome = Some("interrupted".into());
                        sql::statement(
                            "UPDATE agent_attempts SET body=$2,ended_at=NOW() WHERE id=$1",
                        )
                        .bind(attempt_id)
                        .bind(encode(&attempt)?)
                        .exec(&mut tx)
                        .await
                        .map_err(infra)?;
                    }
                }
                run.generation += 1;
                run.version += 1;
                run.status = if unsafe_tool || run.recoveries >= max_recoveries {
                    RunStatus::NeedsAttention
                } else if was_pausing {
                    RunStatus::Paused
                } else {
                    RunStatus::Queued
                };
                if run.status == RunStatus::NeedsAttention {
                    run.error = Some("recovery requires attention".into());
                }
                if run.status == RunStatus::Queued {
                    run.recoveries += 1;
                    run.dispatch += 1;
                    sql::statement(
                        "UPDATE agent_events SET valid=FALSE WHERE run_id=$1 AND draft=TRUE",
                    )
                    .bind(&run.id)
                    .exec(&mut tx)
                    .await
                    .map_err(infra)?;
                    schedule(&mut tx, &run).await?;
                }
                save_run(&mut tx, &run).await?;
                tx.commit().await.map_err(infra)?;
                count += 1;
            }
            Ok(count)
        })
    }
}

async fn unsafe_pending_tool(tx: &mut dyn Executor, run: &Run) -> StoreResult<bool> {
    if !run.checkpoint.tool_inflight {
        return Ok(false);
    }
    let Some(decision) = &run.checkpoint.decision else {
        return Ok(true);
    };
    let calls = decision.tool_calls();
    let Some(call) = calls.get(run.checkpoint.next_tool) else {
        return Ok(true);
    };
    let rows = sql::query("SELECT body FROM agent_tool_executions WHERE run_id=$1 AND call_id=$2")
        .bind(&run.id)
        .bind(&call.call_id)
        .exec(tx)
        .await
        .map_err(infra)?;
    let Some(tool): Option<ToolExecution> = decode(&rows)? else {
        return Ok(true);
    };
    Ok(match tool.recovery.as_str() {
        "safe-to-retry" | "idempotent" => false,
        "reconcilable" => tool.external_id.is_none(),
        _ => true,
    })
}

async fn valid_events(
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

async fn project_assistant(
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
