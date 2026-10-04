//! PostgreSQL infrastructure for durable Agent execution.
//!
//! Business transactions use Toasty; Apalis owns a separate queue schema.
//! A transactional outbox connects the two without sharing connections.

use anyhow::Result;
use toasty::Db;
use toasty_driver_postgresql::PostgreSQL;

pub mod migration;
pub mod pool;
pub mod queue;
pub mod repository;

/// Open a pooled Toasty connection without applying or resetting any schema.
pub async fn connect(database_url: &str) -> Result<Db> {
    connect_with_config(database_url, &pool::PoolConfig::default()).await
}
pub async fn connect_with_config(database_url: &str, config: &pool::PoolConfig) -> Result<Db> {
    config.validate()?;
    Ok(Db::builder()
        .max_pool_size(config.max_connections as usize)
        .pool_wait_timeout(Some(config.wait_timeout))
        .pool_create_timeout(Some(config.connect_timeout))
        .pool_max_connection_lifetime(Some(config.max_lifetime))
        .pool_max_connection_idle_time(Some(config.idle_timeout))
        .build(PostgreSQL::new(database_url)?)
        .await?)
}
