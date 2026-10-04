use anyhow::{Result, ensure};
use sqlx::PgPool;
pub async fn database() -> Result<String> {
    let parent = std::env::var("TEST_DATABASE_URL")?;
    ensure!(
        !parent.contains('?') && parent.ends_with("_test"),
        "TEST_DATABASE_URL must name a dedicated _test database"
    );
    let name = format!("agent_fixture_{}_test", uuid::Uuid::new_v4().simple());
    let pool = PgPool::connect(&parent).await?;
    sqlx::QueryBuilder::<sqlx::Postgres>::new("CREATE DATABASE ")
        .push(&name)
        .build()
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(format!("{}/{name}", parent.rsplit_once('/').unwrap().0))
}
pub async fn cleanup(url: &str) -> Result<()> {
    let name = url.rsplit_once('/').unwrap().1;
    ensure!(
        name.starts_with("agent_fixture_")
            && name.ends_with("_test")
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    );
    let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL")?).await?;
    sqlx::QueryBuilder::<sqlx::Postgres>::new("DROP DATABASE ")
        .push(name)
        .push(" WITH (FORCE)")
        .build()
        .execute(&pool)
        .await?;
    pool.close().await;
    Ok(())
}
