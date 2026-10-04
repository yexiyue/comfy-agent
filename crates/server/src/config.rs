//! Host configuration: deserialize once, then inject validated settings.
use anyhow::{Context, Result, ensure};
use axum::http::{HeaderValue, Uri};
use persistence::pool::PoolConfig;
use serde::Deserialize;
use std::{net::SocketAddr, time::Duration};

#[derive(Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub chat_models: String,
    pub database_url: String,
    pub server_addr: SocketAddr,
    pub cors_allowed_origins: String,
    pub worker_concurrency: usize,
    pub run_lease_seconds: i64,
    pub run_heartbeat_seconds: u64,
    pub run_max_recoveries: usize,
    pub run_event_max_bytes: i64,
    pub db_pool_size: u32,
    pub queue_pool_size: u32,
    pub db_pool_wait_seconds: u64,
    pub db_connect_timeout_seconds: u64,
    pub db_max_lifetime_seconds: u64,
    pub db_idle_timeout_seconds: u64,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            chat_models: String::new(),
            database_url: String::new(),
            server_addr: ([127, 0, 0, 1], 3001).into(),
            cors_allowed_origins: "http://localhost:3000,http://localhost:5173".into(),
            worker_concurrency: 4,
            run_lease_seconds: 30,
            run_heartbeat_seconds: 5,
            run_max_recoveries: 5,
            run_event_max_bytes: 16777216,
            db_pool_size: 16,
            queue_pool_size: 8,
            db_pool_wait_seconds: 10,
            db_connect_timeout_seconds: 5,
            db_max_lifetime_seconds: 1800,
            db_idle_timeout_seconds: 600,
        }
    }
}
impl ServerConfig {
    pub fn from_env() -> Result<Self> {
        let config: Self = envy::from_env().context("invalid server environment configuration")?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.database_url.trim().is_empty(),
            "DATABASE_URL is required; see .env.example"
        );
        ensure!(
            (1..=64).contains(&self.worker_concurrency),
            "WORKER_CONCURRENCY must be 1..=64"
        );
        ensure!(
            self.run_heartbeat_seconds > 0
                && self.run_lease_seconds > 0
                && self.run_heartbeat_seconds < self.run_lease_seconds as u64,
            "heartbeat must be positive and less than lease"
        );
        ensure!(
            self.run_max_recoveries > 0 && self.run_event_max_bytes > 0,
            "recovery and event limits must be positive"
        );
        self.pool(self.db_pool_size).validate()?;
        self.pool(self.queue_pool_size).validate()?;
        self.origins()?;
        Ok(())
    }
    pub fn pool(&self, max_connections: u32) -> PoolConfig {
        PoolConfig {
            max_connections,
            wait_timeout: Duration::from_secs(self.db_pool_wait_seconds),
            connect_timeout: Duration::from_secs(self.db_connect_timeout_seconds),
            max_lifetime: Duration::from_secs(self.db_max_lifetime_seconds),
            idle_timeout: Duration::from_secs(self.db_idle_timeout_seconds),
        }
    }
    pub fn worker(&self) -> runtime::execution::WorkerConfig {
        runtime::execution::WorkerConfig {
            lease_seconds: self.run_lease_seconds,
            heartbeat: Duration::from_secs(self.run_heartbeat_seconds),
            max_recoveries: self.run_max_recoveries,
            max_event_bytes: self.run_event_max_bytes,
        }
    }
    pub fn origins(&self) -> Result<Vec<HeaderValue>> {
        self.cors_allowed_origins
            .split(',')
            .map(|origin| {
                let origin = origin.trim();
                ensure!(
                    origin.starts_with("http://") || origin.starts_with("https://"),
                    "CORS origins must be explicit HTTP(S) origins"
                );
                let uri: Uri = origin.parse().context("invalid CORS origin URI")?;
                let authority = uri.authority().context("CORS origin requires a host")?;
                ensure!(
                    !authority.as_str().contains(['*', '@'])
                        && origin
                            == format!("{}://{authority}", uri.scheme_str().unwrap_or_default()),
                    "CORS origins must contain only scheme, host, and optional port"
                );
                origin.parse().context("invalid CORS origin")
            })
            .collect()
    }
}
