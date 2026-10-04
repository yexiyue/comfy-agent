//! PostgreSQL infrastructure for durable Agent execution.
//!
//! Business transactions use Toasty; Apalis owns a separate queue schema.
//! A transaction outbox will connect the two without sharing connections.

use anyhow::Result;
use toasty::Db;
use toasty_driver_postgresql::PostgreSQL;

pub mod migration;
pub mod queue;
pub mod repository;

/// Open a pooled Toasty connection without applying or resetting any schema.
pub async fn connect(database_url: &str) -> Result<Db> {
    Ok(Db::builder().build(PostgreSQL::new(database_url)?).await?)
}
