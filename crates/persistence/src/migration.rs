//! Explicit versioned migrations. Opening the repository never mutates schema.

use anyhow::{Result, bail, ensure};
use toasty::{Db, sql, stmt::Value};

pub const SCHEMA_VERSION: i64 = 1;
const UP: &str = include_str!("../migrations/0001_durable_sessions.sql");
const TABLES: &[&str] = &[
    "agent_outbox",
    "agent_commands",
    "agent_events",
    "agent_tool_executions",
    "agent_attempts",
    "agent_messages",
    "agent_runs",
    "agent_conversations",
];

pub(crate) fn text_column(row: &Value, index: usize) -> Result<&str> {
    if let Value::Record(record) = row
        && let Some(Value::String(value)) = record.get(index)
    {
        return Ok(value);
    }
    bail!("unexpected database result type")
}

pub async fn check(db: &mut Db) -> Result<()> {
    let rows = sql::query("SELECT version::text FROM agent_schema_version WHERE singleton = TRUE")
        .exec(db)
        .await?;
    ensure!(
        rows.len() == 1 && text_column(&rows[0], 0)? == SCHEMA_VERSION.to_string(),
        "incompatible database schema; run the explicit migration command"
    );
    Ok(())
}

pub async fn migrate(db: &mut Db) -> Result<()> {
    let mut tx = db.transaction().await?;
    sql::statement("SELECT pg_advisory_xact_lock(348294057)")
        .exec(&mut tx)
        .await?;
    sql::statement("CREATE TABLE IF NOT EXISTS agent_schema_version (singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK(singleton), version BIGINT NOT NULL)").exec(&mut tx).await?;
    let rows = sql::query("SELECT version::text FROM agent_schema_version")
        .exec(&mut tx)
        .await?;
    if let Some(row) = rows.first() {
        ensure!(
            text_column(row, 0)? == SCHEMA_VERSION.to_string(),
            "unsupported schema version"
        );
    } else {
        // This fixed migration contains statements only, no procedural SQL.
        for statement in UP.split(';').map(str::trim).filter(|sql| !sql.is_empty()) {
            sql::statement(statement).exec(&mut tx).await?;
        }
        sql::statement("INSERT INTO agent_schema_version VALUES (TRUE, $1)")
            .bind(SCHEMA_VERSION)
            .exec(&mut tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Only an empty application schema can be rolled back; existing data is kept.
pub async fn rollback_empty(db: &mut Db) -> Result<()> {
    check(db).await?;
    let mut tx = db.transaction().await?;
    sql::statement("SELECT pg_advisory_xact_lock(348294057)")
        .exec(&mut tx)
        .await?;
    for table in TABLES {
        sql::statement(format!("LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE"))
            .exec(&mut tx)
            .await?;
        let rows = sql::query(format!("SELECT 1::text FROM {table} LIMIT 1"))
            .exec(&mut tx)
            .await?;
        ensure!(
            rows.is_empty(),
            "rollback refused: application schema contains data"
        );
    }
    for table in TABLES {
        sql::statement(format!("DROP TABLE {table}"))
            .exec(&mut tx)
            .await?;
    }
    sql::statement("DROP TABLE agent_schema_version")
        .exec(&mut tx)
        .await?;
    tx.commit().await?;
    Ok(())
}
