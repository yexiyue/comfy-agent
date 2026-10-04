//! Run explicitly against an isolated database:
//! TEST_DATABASE_URL=... cargo test -p persistence --test compatibility -- --ignored

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use apalis::prelude::*;
use apalis_postgres::{Config, PgPool, PostgresStorage, queries};
use toasty::{Db, sql};
use uuid::Uuid;

fn test_url() -> Result<String> {
    let url = std::env::var("TEST_DATABASE_URL")
        .context("set TEST_DATABASE_URL to an isolated database ending in _test")?;
    let database = url.split('?').next().unwrap_or(&url).rsplit('/').next();
    ensure!(
        database.is_some_and(|name| name.ends_with("_test")),
        "compatibility tests require a database name ending in _test"
    );
    Ok(url)
}

async fn setup_probe() -> Result<Db> {
    let mut db = persistence::connect(&test_url()?).await?;
    sql::statement(
        "CREATE TABLE IF NOT EXISTS compatibility_probe (
          id TEXT PRIMARY KEY, version BIGINT NOT NULL, body TEXT NOT NULL)",
    )
    .exec(&mut db)
    .await?;
    Ok(db)
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn configured_business_and_queue_pools_bound_acquisition_and_reuse_connections() -> Result<()>
{
    let config = persistence::pool::PoolConfig {
        max_connections: 1,
        wait_timeout: Duration::from_millis(100),
        ..Default::default()
    };
    let db = persistence::connect_with_config(&test_url()?, &config).await?;
    let held = db.connection().await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(2), db.connection())
            .await?
            .is_err()
    );
    drop(held);
    drop(db.connection().await?);

    let queue = config.queue(&test_url()?).await?;
    let held = queue.acquire().await?;
    assert!(
        tokio::time::timeout(Duration::from_secs(2), queue.acquire())
            .await?
            .is_err()
    );
    drop(held);
    drop(queue.acquire().await?);
    queue.close().await;
    Ok(())
}

async fn insert(db: &mut dyn toasty::Executor, id: &str) -> Result<()> {
    sql::statement("INSERT INTO compatibility_probe VALUES ($1, 0, 'initial')")
        .bind(id)
        .exec(db)
        .await?;
    Ok(())
}

async fn count(db: &mut Db, id: &str) -> Result<usize> {
    Ok(
        sql::query("SELECT id FROM compatibility_probe WHERE id = $1")
            .bind(id)
            .exec(db)
            .await?
            .len(),
    )
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn transaction_commit_error_and_drop_are_atomic() -> Result<()> {
    let mut db = setup_probe().await?;
    let committed = Uuid::new_v4().to_string();
    let dropped = Uuid::new_v4().to_string();
    let failed = Uuid::new_v4().to_string();
    {
        let mut tx = db.transaction().await?;
        insert(&mut tx, &committed).await?;
        tx.commit().await?;
    }
    {
        let mut tx = db.transaction().await?;
        insert(&mut tx, &dropped).await?;
    }
    {
        let mut tx = db.transaction().await?;
        insert(&mut tx, &failed).await?;
        assert!(insert(&mut tx, &committed).await.is_err());
        tx.rollback().await?;
    }
    assert_eq!(count(&mut db, &committed).await?, 1);
    assert_eq!(count(&mut db, &dropped).await?, 0);
    assert_eq!(count(&mut db, &failed).await?, 0);
    sql::statement("DELETE FROM compatibility_probe WHERE id = $1")
        .bind(committed)
        .exec(&mut db)
        .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn cancelled_future_rolls_back_before_pool_reuse() -> Result<()> {
    let mut db = setup_probe().await?;
    let id = Uuid::new_v4().to_string();
    let task_id = id.clone();
    let mut other = db.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut tx = other.transaction().await?;
        insert(&mut tx, &task_id).await?;
        let _ = started.send(());
        std::future::pending::<()>().await;
        tx.commit().await?;
        Ok::<_, anyhow::Error>(())
    });
    tokio::time::timeout(Duration::from_secs(5), ready).await??;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(count(&mut db, &id).await?, 0);
    insert(&mut db, &id).await?;
    sql::statement("DELETE FROM compatibility_probe WHERE id = $1")
        .bind(id)
        .exec(&mut db)
        .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn conditional_claim_allows_only_one_writer() -> Result<()> {
    let mut db = setup_probe().await?;
    let id = Uuid::new_v4().to_string();
    insert(&mut db, &id).await?;
    let first = db.clone();
    let second = db.clone();
    let claim = |mut db: Db, id: String| async move {
        sql::statement(
            "UPDATE compatibility_probe SET version = version + 1 WHERE id = $1 AND version = 0",
        )
        .bind(id)
        .exec(&mut db)
        .await
    };
    let (left, right) = tokio::join!(
        claim(first.clone(), id.clone()),
        claim(second.clone(), id.clone())
    );
    assert_eq!(left? + right?, 1);
    // Duplicate delivery and a stale writer cannot claim the old generation.
    assert_eq!(claim(first.clone(), id.clone()).await?, 0);
    assert_eq!(claim(second.clone(), id.clone()).await?, 0);
    sql::statement("DELETE FROM compatibility_probe WHERE id = $1")
        .bind(id)
        .exec(&mut db)
        .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL"]
async fn postgres_queue_delivers_duplicates_and_recovers_orphans() -> Result<()> {
    let pool = PgPool::connect(&test_url()?).await?;
    PostgresStorage::setup(&pool).await?;
    // Migrations are explicit and idempotent.
    PostgresStorage::setup(&pool).await?;
    let queue = format!("compatibility-{}", Uuid::new_v4());
    let mut backend = PostgresStorage::<String>::new(&pool)
        .with_config(Config::default().queue(&queue).batch_size(1));
    let payload = Uuid::new_v4().to_string();
    backend.push(payload.clone()).await?;
    backend.push(payload).await?;
    let mut conn = pool.acquire().await?;
    let worker_name = format!("probe-{}", Uuid::new_v4());
    let worker = WorkerContext::new(&worker_name);
    // Simulate durable facts left by a worker that died after receiving a batch.
    sqlx::query("INSERT INTO apalis.workers (id, worker_type, storage_name, layers, last_seen) VALUES ($1, $2, 'PgStorage', '', NOW() - INTERVAL '1 hour')")
        .bind(worker.name())
        .bind(&queue)
        .execute(&mut *conn)
        .await?;
    let config = Config::default().queue(&queue).batch_size(1);
    let first = queries::fetch_next(&mut *conn, &config, &worker).await?;
    assert_eq!(first.len(), 1);
    let second = queries::fetch_next(&mut *conn, &config, &worker).await?;
    assert_eq!(second.len(), 1);
    assert_eq!(queries::reenqueue_orphaned(&mut *conn, &queue, 1).await?, 2);
    let reclaimed = queries::fetch_next(&mut *conn, &config, &worker).await?;
    assert_eq!(reclaimed.len(), 1);
    sqlx::query("DELETE FROM apalis.jobs WHERE job_type = $1")
        .bind(&queue)
        .execute(&mut *conn)
        .await?;
    sqlx::query("DELETE FROM apalis.workers WHERE id = $1")
        .bind(worker.name())
        .execute(&mut *conn)
        .await?;
    Ok(())
}
