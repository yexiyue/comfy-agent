use anyhow::Result;
use genai::chat::ChatRequest;
use persistence::{migration, repository::PostgresStore};
use runtime::{
    model::*,
    store::{ConversationStore, StoreError},
};
use serde_json::json;
use toasty::sql;
use uuid::Uuid;

fn url() -> Result<String> {
    let url = std::env::var("TEST_DATABASE_URL")?;
    anyhow::ensure!(
        url.split('?').next().unwrap().ends_with("_test"),
        "isolated test database required"
    );
    Ok(url)
}
async fn store() -> Result<PostgresStore> {
    let mut db = persistence::connect(&url()?).await?;
    migration::migrate(&mut db).await?;
    PostgresStore::open(&url()?).await
}
async fn conversation(store: &PostgresStore) -> Result<Conversation> {
    Ok(store
        .create(Conversation {
            id: Uuid::new_v4().to_string(),
            revision: 0,
            messages: vec![],
            history: ChatRequest::default(),
            active_run_id: None,
            parent_id: None,
        })
        .await?)
}
fn submit(conversation: &Conversation) -> Submit {
    Submit {
        conversation_id: conversation.id.clone(),
        expected_revision: conversation.revision,
        request_id: Uuid::new_v4().to_string(),
        message: UiMessage {
            id: Uuid::new_v4().to_string(),
            role: "user".into(),
            parts: vec![json!({"type":"text","text":"hello"})],
            metadata: None,
        },
        model: "mock-model".into(),
        max_steps: 3,
        tool_schema_hash: "mock-schema".into(),
        evaluation: false,
        traceparent: None,
        tracestate: None,
    }
}
async fn cleanup(id: &str) -> Result<()> {
    let mut db = persistence::connect(&url()?).await?;
    let mut tx = db.transaction().await?;
    for table in [
        "agent_outbox",
        "agent_events",
        "agent_tool_executions",
        "agent_attempts",
    ] {
        sql::statement(format!("DELETE FROM {table} WHERE run_id IN (SELECT id FROM agent_runs WHERE conversation_id=$1)")).bind(id).exec(&mut tx).await?;
    }
    sql::statement("DELETE FROM agent_commands WHERE scope=$1")
        .bind(id)
        .exec(&mut tx)
        .await?;
    sql::statement("DELETE FROM agent_messages WHERE conversation_id=$1")
        .bind(id)
        .exec(&mut tx)
        .await?;
    sql::statement("DELETE FROM agent_runs WHERE conversation_id=$1")
        .bind(id)
        .exec(&mut tx)
        .await?;
    sql::statement("DELETE FROM agent_conversations WHERE id=$1")
        .bind(id)
        .exec(&mut tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn claim_waits_for_conversation_before_locking_run() -> Result<()> {
    let store = store().await?;
    let conversation = conversation(&store).await?;
    let run = store.submit(submit(&conversation)).await?;
    let mut db = persistence::connect(&url()?).await?;
    let mut tx = db.transaction().await?;
    sql::query("SELECT id FROM agent_conversations WHERE id=$1 FOR UPDATE")
        .bind(&conversation.id)
        .exec(&mut tx)
        .await?;
    let claimant = store.clone();
    let task = Dispatch {
        run_id: run.id.clone(),
        dispatch: run.dispatch,
    };
    let claim = tokio::spawn(async move { claimant.claim(task, 30).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!claim.is_finished());
    // A reader holding the conversation must still be able to take the run lock.
    sql::query("SELECT id FROM agent_runs WHERE id=$1 FOR UPDATE NOWAIT")
        .bind(&run.id)
        .exec(&mut tx)
        .await?;
    tx.commit().await?;
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), claim)
            .await???
            .is_some()
    );
    cleanup(&conversation.id).await
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn accepts_once_rejects_conflicts_and_rolls_back_partial_work() -> Result<()> {
    let store = store().await?;
    let conversation = conversation(&store).await?;
    let command = submit(&conversation);
    let run = store.submit(command.clone()).await?;
    assert_eq!(store.submit(command.clone()).await?.id, run.id);
    let mut changed = command;
    changed.message.parts = vec![json!({"type":"text","text":"different"})];
    assert!(matches!(
        store.submit(changed).await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        store.submit(submit(&conversation)).await,
        Err(StoreError::Conflict)
    ));
    let saved = store.conversation(&conversation.id).await?;
    assert_eq!(saved.messages.len(), 1);
    assert_eq!(saved.revision, 1);
    let cancel = Control {
        run_id: run.id.clone(),
        expected_version: run.version,
        request_id: Uuid::new_v4().to_string(),
        action: ControlAction::Cancel,
    };
    assert_eq!(
        store.control(cancel.clone()).await?.status,
        RunStatus::Cancelled
    );
    assert_eq!(store.control(cancel).await?.status, RunStatus::Cancelled);
    let saved = store.conversation(&conversation.id).await?;
    let mut invalid = submit(&saved);
    invalid.max_steps = 0;
    assert!(store.submit(invalid).await.is_err());
    assert_eq!(
        store.conversation(&conversation.id).await?.messages.len(),
        1
    );
    cleanup(&conversation.id).await
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn competing_submissions_and_claims_have_one_winner() -> Result<()> {
    let store = store().await?;
    let conversation = conversation(&store).await?;
    let (left, right) = tokio::join!(
        store.submit(submit(&conversation)),
        store.submit(submit(&conversation))
    );
    assert_ne!(left.is_ok(), right.is_ok());
    let run = left.or(right)?;
    let task = Dispatch {
        run_id: run.id.clone(),
        dispatch: run.dispatch,
    };
    let (left, right) = tokio::join!(store.claim(task.clone(), 30), store.claim(task.clone(), 30));
    assert_eq!(
        usize::from(left?.is_some()) + usize::from(right?.is_some()),
        1
    );
    assert!(store.claim(task, 30).await?.is_none());
    cleanup(&conversation.id).await
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn paused_run_can_resume_or_be_atomically_superseded() -> Result<()> {
    let store = store().await?;
    let conversation = conversation(&store).await?;
    let run = store.submit(submit(&conversation)).await?;
    let paused = store
        .control(Control {
            run_id: run.id.clone(),
            expected_version: run.version,
            request_id: Uuid::new_v4().to_string(),
            action: ControlAction::Pause,
        })
        .await?;
    assert_eq!(paused.status, RunStatus::Paused);
    let resumed = store
        .control(Control {
            run_id: run.id.clone(),
            expected_version: paused.version,
            request_id: Uuid::new_v4().to_string(),
            action: ControlAction::Resume,
        })
        .await?;
    assert_eq!(resumed.id, run.id);
    assert!(resumed.dispatch > run.dispatch);
    let paused = store
        .control(Control {
            run_id: run.id.clone(),
            expected_version: resumed.version,
            request_id: Uuid::new_v4().to_string(),
            action: ControlAction::Pause,
        })
        .await?;
    let snapshot = store.conversation(&conversation.id).await?;
    let input = submit(&snapshot).message;
    let command = Control {
        run_id: run.id.clone(),
        expected_version: paused.version,
        request_id: Uuid::new_v4().to_string(),
        action: ControlAction::Steer {
            message: input,
            expected_revision: snapshot.revision,
        },
    };
    let next = store.control(command.clone()).await?;
    assert_ne!(next.id, run.id);
    assert_eq!(next.supersedes.as_deref(), Some(run.id.as_str()));
    assert_eq!(store.control(command).await?.id, next.id);
    assert_eq!(store.run(&run.id).await?.status, RunStatus::Superseded);
    assert_eq!(
        store.conversation(&conversation.id).await?.messages.len(),
        2
    );
    assert_eq!(next.checkpoint.history.messages.len(), 2);
    cleanup(&conversation.id).await
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn phase_and_events_are_atomic_and_stale_generations_are_fenced() -> Result<()> {
    let store = store().await?;
    let conversation = conversation(&store).await?;
    let run = store.submit(submit(&conversation)).await?;
    let mut run = store
        .claim(
            Dispatch {
                run_id: run.id.clone(),
                dispatch: run.dispatch,
            },
            30,
        )
        .await?
        .unwrap();
    let attempt = Attempt {
        id: run.attempt_id.clone().unwrap(),
        run_id: run.id.clone(),
        generation: run.generation,
        outcome: None,
        trace_id: None,
        model_calls: 1,
        tool_calls: 0,
        known_tokens: 0,
        usage_complete: false,
    };
    let event = ProgressEvent {
        sequence: 0,
        attempt_id: attempt.id.clone(),
        step: 1,
        draft: true,
        payload: json!({"type":"text-delta","id":"text-1","delta":"hello"}),
    };
    let tool = ToolExecution {
        call_id: "atomic-result".into(),
        step: 1,
        name: "add".into(),
        arguments: json!({}),
        recovery: "safe-to-retry".into(),
        operation_key: "atomic-key".into(),
        external_id: None,
        output: Some(json!({"sum":8})),
        is_error: false,
    };
    run.checkpoint.step = 1;
    run.checkpoint.model_inflight = true;
    assert!(
        store
            .commit(
                run.clone(),
                attempt.clone(),
                vec![event.clone()],
                vec![tool.clone()],
                1
            )
            .await
            .is_err()
    );
    assert_eq!(store.run(&run.id).await?.checkpoint.step, 0);
    assert!(store.events(&run.id, 0, 100).await?.is_empty());
    assert!(store.tool(&run.id, "atomic-result").await?.is_none());
    let committed = store
        .commit(
            run.clone(),
            attempt.clone(),
            vec![event.clone()],
            vec![tool],
            16384,
        )
        .await?;
    assert_eq!(committed.checkpoint.step, 1);
    assert_eq!(
        store.tool(&run.id, "atomic-result").await?.unwrap().output,
        Some(json!({"sum":8}))
    );
    assert_eq!(store.events(&run.id, 0, 100).await?.len(), 1);
    let controlled = store
        .control(Control {
            run_id: run.id.clone(),
            expected_version: committed.version,
            request_id: Uuid::new_v4().to_string(),
            action: ControlAction::Pause,
        })
        .await?;
    assert_eq!(controlled.status, RunStatus::Pausing);
    assert!(!store.heartbeat(&run.id, run.generation, 30).await?);
    assert!(matches!(
        store
            .commit(committed, attempt, vec![event.clone()], vec![], 16384)
            .await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        store
            .append(&run.id, run.generation, vec![event], 16384)
            .await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(store.events(&run.id, 0, 100).await?.len(), 1);
    let mut db = persistence::connect(&url()?).await?;
    let cancelled = store
        .control(Control {
            run_id: run.id.clone(),
            expected_version: controlled.version,
            request_id: Uuid::new_v4().to_string(),
            action: ControlAction::Cancel,
        })
        .await?;
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    let snapshot = store.conversation(&conversation.id).await?;
    let queued = store.submit(submit(&snapshot)).await?;
    let expired = store
        .claim(
            Dispatch {
                run_id: queued.id.clone(),
                dispatch: queued.dispatch,
            },
            30,
        )
        .await?
        .unwrap();
    sql::statement("UPDATE agent_runs SET lease_until=NOW()-INTERVAL '1 second' WHERE id=$1")
        .bind(&expired.id)
        .exec(&mut db)
        .await?;
    assert!(!store.heartbeat(&expired.id, expired.generation, 30).await?);
    assert!(matches!(
        store
            .append(&expired.id, expired.generation, vec![], 16384)
            .await,
        Err(StoreError::Conflict)
    ));
    cleanup(&conversation.id).await
}
