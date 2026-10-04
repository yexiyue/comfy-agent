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
    let database_url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL is required; see .env.example and run the explicit migration")?;
    let store=Arc::new(persistence::repository::PostgresStore::open(&database_url).await.map_err(|_|anyhow::anyhow!("database connection or schema check failed; verify DATABASE_URL and run migrations"))?);
    let registry = Arc::new(default_registry()?);
    let worker_config = runtime::execution::WorkerConfig {
        lease_seconds: env_number("RUN_LEASE_SECONDS", 30)?,
        heartbeat: std::time::Duration::from_secs(env_number("RUN_HEARTBEAT_SECONDS", 5)?),
        max_recoveries: env_number("RUN_MAX_RECOVERIES", 5)?,
        max_event_bytes: env_number("RUN_EVENT_MAX_BYTES", 16777216)?,
    };
    let mut service = runtime::execution::ExecutionService::new(
        store,
        Arc::new(runtime::execution::GenaiGateway(config.build_client()?)),
        registry,
        worker_config,
        shutdown.clone(),
    )?;
    service.telemetry = telemetry_config;
    use sha2::{Digest, Sha256};
    let tool_schema_hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&service.registry.definitions())?)
    );
    service.expected_config = Some((config.model.clone(), tool_schema_hash.clone()));
    let service = Arc::new(service);
    let queue = persistence::queue::QueueRuntime::open(
        &database_url,
        &service,
        env_number("WORKER_CONCURRENCY", 4)?,
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!("queue initialization failed; verify database and Apalis migrations")
    })?;
    let state = AppState {
        service: service.clone(),
        model: config.model.into(),
        max_steps: config.max_steps,
        tool_schema_hash: tool_schema_hash.into(),
    };
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(address = %listener.local_addr()?, "chat server listening");
    if let Ok(path) = std::env::var("AGENT_CONFIG_MANIFEST") {
        use sha2::{Digest, Sha256};
        let schemas = serde_json::to_value(state.service.registry.definitions())?;
        let hash = |v: &str| format!("{:x}", Sha256::digest(v.as_bytes()));
        let manifest = serde_json::json!({"model":state.model.as_ref(),"maxSteps":state.max_steps,"toolSchemaHash":hash(&schemas.to_string()),"systemPromptHash":hash(""),"promptSource":"server-conversation","toolSchemas":schemas});
        std::fs::write(path, serde_json::to_vec_pretty(&manifest)?)?;
    }
    let server = axum::serve(listener, router(state, origins)).with_graceful_shutdown({
        let shutdown = shutdown.clone();
        async move { shutdown.cancelled().await }
    });
    let signals = async {
        if let Err(error) = shutdown_signal().await {
            tracing::error!(%error,"shutdown signal failed");
        }
        shutdown.cancel();
    };
    let execution = async {
        let result = queue.run(service).await;
        shutdown.cancel();
        result
    };
    // Poll worker/server in the same runtime without imposing additional Send bounds on Apalis.
    let work = async {
        let (http, worker) = tokio::join!(server, execution);
        http?;
        worker?;
        Ok::<_, anyhow::Error>(())
    };
    tokio::pin!(work);
    tokio::pin!(signals);
    let result = tokio::select! { result=&mut work=>result,_=&mut signals=>{
        match tokio::time::timeout(std::time::Duration::from_secs(10),&mut work).await {
            Ok(result)=>result,Err(_)=>{tracing::warn!("shutdown grace expired; outstanding leases will recover after expiry");Ok(())}
        }
    }};
    result?;
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

fn env_number<T: std::str::FromStr>(name: &str, default: T) -> anyhow::Result<T> {
    match std::env::var(name) {
        Ok(value) => value.parse().map_err(|_| anyhow::anyhow!("invalid {name}")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(_) => Err(anyhow::anyhow!("invalid {name}")),
    }
}
