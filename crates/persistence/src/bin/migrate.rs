use anyhow::{Context, Result, ensure};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.is_empty() || args == ["--rollback-empty"],
        "usage: migrate [--rollback-empty]"
    );
    let url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let mut db = persistence::connect(&url).await?;
    if args.is_empty() {
        persistence::migration::migrate(&mut db).await?;
        let pool = apalis_postgres::PgPool::connect(&url).await?;
        apalis_postgres::PostgresStorage::setup(&pool).await?;
        println!("Business schema v1 and Apalis migrations applied");
    } else {
        persistence::migration::rollback_empty(&mut db).await?;
        println!("Empty business schema rolled back; Apalis data retained");
    }
    Ok(())
}
