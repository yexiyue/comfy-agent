use std::{net::SocketAddr, sync::Arc};

use agent::AgentConfig;
use anyhow::Context;
use axum::http::{HeaderValue, Uri};
use server::{AppState, default_registry, router};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match dotenvy::dotenv() {
        Ok(_) => {}
        Err(dotenvy::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("failed to load .env"),
    }
    let telemetry_config = Arc::new(telemetry::Config::from_env()?);
    let provider = telemetry::init(&telemetry_config)?;
    let config = AgentConfig::from_env()?;
    let address: SocketAddr = std::env::var("SERVER_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:3001".into())
        .parse()
        .context("invalid SERVER_ADDR")?;
    let origins = std::env::var("CORS_ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "http://localhost:3000,http://localhost:5173".into())
        .split(',')
        .map(|origin| {
            let origin = origin.trim();
            anyhow::ensure!(
                origin.starts_with("http://") || origin.starts_with("https://"),
                "CORS origins must be explicit HTTP(S) origins"
            );
            let uri: Uri = origin.parse().context("invalid CORS origin URI")?;
            let authority = uri.authority().context("CORS origin requires a host")?;
            anyhow::ensure!(
                !authority.as_str().contains(['*', '@'])
                    && origin == format!("{}://{authority}", uri.scheme_str().unwrap_or_default()),
                "CORS origins must contain only scheme, host, and optional port"
            );
            origin.parse::<HeaderValue>().context("invalid CORS origin")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let shutdown = CancellationToken::new();
    let tasks = tokio_util::task::TaskTracker::new();
    let state = AppState {
        client: config.build_client()?,
        model: config.model.into(),
        max_steps: config.max_steps,
        registry: Arc::new(default_registry()?),
        shutdown: shutdown.clone(),
        telemetry: telemetry_config,
        tasks: tasks.clone(),
    };
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(address = %listener.local_addr()?, "chat server listening");
    if let Ok(path) = std::env::var("AGENT_CONFIG_MANIFEST") {
        use sha2::{Digest, Sha256};
        let schemas = serde_json::to_value(state.registry.definitions())?;
        let hash = |v: &str| format!("{:x}", Sha256::digest(v.as_bytes()));
        let manifest = serde_json::json!({"model":state.model.as_ref(),"maxSteps":state.max_steps,"toolSchemaHash":hash(&schemas.to_string()),"systemPromptHash":hash(""),"promptSource":"client-history","toolSchemas":schemas});
        std::fs::write(path, serde_json::to_vec_pretty(&manifest)?)?;
    }
    axum::serve(listener, router(state, origins))
        .with_graceful_shutdown(async move {
            if let Err(error) = shutdown_signal().await {
                tracing::error!(%error, "failed to listen for shutdown signal");
            }
            shutdown.cancel();
        })
        .await?;
    tasks.close();
    tasks.wait().await;
    telemetry::shutdown(provider).await;
    Ok(())
}

async fn shutdown_signal() -> std::io::Result<()> {
    if std::env::var("SERVER_SHUTDOWN_STDIN").as_deref() == Ok("true") {
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::stdin().lock().lines() {
                if matches!(line.as_deref(), Ok("shutdown")) {
                    let _ = tx.send(());
                    break;
                }
            }
        });
        tokio::select! {
            _ = rx => return Ok(()),
            result = tokio::signal::ctrl_c() => return result,
        }
    }
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
