//! Connection budgets and bounded acquisition shared by business and queue pools.
use anyhow::{Context, Result, ensure};
use std::time::Duration;

#[derive(Clone)]
pub struct PoolConfig {
    pub max_connections: u32,
    pub wait_timeout: Duration,
    pub connect_timeout: Duration,
    pub max_lifetime: Duration,
    pub idle_timeout: Duration,
}
impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 16,
            wait_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(5),
            max_lifetime: Duration::from_secs(1800),
            idle_timeout: Duration::from_secs(600),
        }
    }
}
impl PoolConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=1024).contains(&self.max_connections),
            "pool size must be 1..=1024"
        );
        ensure!(
            !self.wait_timeout.is_zero()
                && !self.connect_timeout.is_zero()
                && !self.max_lifetime.is_zero()
                && !self.idle_timeout.is_zero(),
            "pool timeouts must be positive"
        );
        Ok(())
    }
    pub async fn queue(&self, url: &str) -> Result<sqlx::PgPool> {
        self.validate()?;
        Ok(tokio::time::timeout(
            self.connect_timeout,
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(self.max_connections)
                .acquire_timeout(self.wait_timeout)
                .max_lifetime(self.max_lifetime)
                .idle_timeout(self.idle_timeout)
                .connect(url),
        )
        .await
        .context("queue database connection timed out")??)
    }
}
