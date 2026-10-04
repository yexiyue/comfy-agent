//! At-least-once delivery through an outbox, with business-owned execution leases.

use anyhow::{Result, ensure};
use apalis::prelude::*;
use apalis_postgres::{Config, PostgresStorage};
use runtime::{execution::ExecutionService, model::Dispatch};
use std::sync::Arc;

pub struct QueueRuntime {
    backend: PostgresStorage<Dispatch>,
    concurrency: usize,
}

impl QueueRuntime {
    pub async fn open(
        url: &str,
        service: &ExecutionService,
        concurrency: usize,
        pool_config: &crate::pool::PoolConfig,
    ) -> Result<Self> {
        ensure!(
            concurrency > 0 && concurrency <= 64,
            "worker concurrency must be between 1 and 64"
        );
        let pool = pool_config.queue(url).await?;
        let exists: bool = sqlx::query_scalar("SELECT to_regclass('apalis.jobs') IS NOT NULL")
            .fetch_one(&pool)
            .await?;
        ensure!(
            exists,
            "Apalis schema is missing; run the explicit migration command"
        );
        // Apalis' PostgreSQL orphan timeout is expressed in whole seconds.
        let queue_heartbeat = service
            .config
            .heartbeat
            .max(std::time::Duration::from_secs(1));
        let config = Config::default()
            .queue("comfy-agent-runs")
            .batch_size(1)
            .heartbeat_interval(queue_heartbeat)
            .missed_heartbeats(3)
            .persist_results(false);
        Ok(Self {
            backend: PostgresStorage::new(&pool).with_config(config),
            concurrency,
        })
    }

    pub async fn run(self, service: Arc<ExecutionService>) -> Result<()> {
        let context = WorkerContext::new(&format!("agent-worker-{}", uuid::Uuid::new_v4()));
        let worker = WorkerBuilder::new(&context)
            .backend(self.backend.clone())
            .concurrency(self.concurrency)
            .data(service.clone())
            .build(dispatch);
        let mut backend = self.backend;
        let publishing = async {
            let mut timer = tokio::time::interval(std::time::Duration::from_millis(500));
            loop {
                tokio::select! { _=service.shutdown.cancelled()=>break,_=timer.tick()=>{} }
                if let Err(error) = service.store.recover(service.config.max_recoveries).await {
                    tracing::error!(%error,"run recovery scan failed");
                }
                match service.store.outbox(64).await {
                    Ok(tasks) => {
                        for task in tasks {
                            if let Err(error) = backend.push(task.clone()).await {
                                tracing::error!(%error,"outbox publication failed");
                                break;
                            }
                            if let Err(error) = service.store.published(task).await {
                                tracing::error!(%error,"outbox acknowledgement failed");
                                break;
                            }
                        }
                    }
                    Err(error) => tracing::error!(%error,"outbox scan failed"),
                }
            }
            context.stop()?;
            Ok::<_, anyhow::Error>(())
        };
        let running = worker.run();
        tokio::pin!(running);
        tokio::pin!(publishing);
        tokio::select! {
            result=&mut running=>{service.shutdown.cancel();publishing.await?;result.map_err(|error|anyhow::anyhow!(error.to_string()))?;},
            result=&mut publishing=>{result?;running.await.map_err(|error|anyhow::anyhow!(error.to_string()))?;},
        }
        Ok(())
    }
}

async fn dispatch(
    task: Dispatch,
    service: Data<Arc<ExecutionService>>,
) -> Result<(), std::io::Error> {
    Arc::clone(&service)
        .execute(task)
        .await
        .map_err(std::io::Error::other)
}
