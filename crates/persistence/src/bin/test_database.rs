//! Test fixture provisioning only; never accepts a business database as the parent.
use anyhow::{Context, ensure};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let parent = std::env::var("TEST_DATABASE_URL").context("TEST_DATABASE_URL is required")?;
    ensure!(
        !parent.contains('?') && parent.ends_with("_test"),
        "test parent must end in _test and contain no query parameters"
    );
    let pool = sqlx::PgPool::connect(&parent).await?;
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    match arguments.as_slice() {
        [action] if action == "create" => {
            let name = format!("agent_fixture_{}_test", uuid::Uuid::new_v4().simple());
            sqlx::QueryBuilder::<sqlx::Postgres>::new("CREATE DATABASE ")
                .push(&name)
                .build()
                .execute(&pool)
                .await?;
            println!("{name}");
        }
        [action, name] if action == "drop" => {
            ensure!(
                name.starts_with("agent_fixture_")
                    && name.ends_with("_test")
                    && name.len() == 51
                    && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "invalid generated fixture database name"
            );
            sqlx::QueryBuilder::<sqlx::Postgres>::new("DROP DATABASE ")
                .push(name)
                .push(" WITH (FORCE)")
                .build()
                .execute(&pool)
                .await?;
        }
        _ => anyhow::bail!("usage: test_database create | drop <generated-name>"),
    }
    pool.close().await;
    Ok(())
}
