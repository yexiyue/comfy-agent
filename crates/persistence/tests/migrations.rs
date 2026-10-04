mod common;
use anyhow::Result;
use persistence::migration;
use toasty::sql;

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL; run serially"]
async fn migrations_are_explicit_versioned_and_rollback_only_empty_data() -> Result<()> {
    let url = common::database().await?;
    assert!(url.split('?').next().unwrap().ends_with("_test"));
    let mut db = persistence::connect(&url).await?;
    migration::migrate(&mut db).await?;
    migration::migrate(&mut db).await?;
    migration::check(&mut db).await?;
    {
        let mut tx = db.transaction().await?;
        sql::statement("UPDATE agent_schema_version SET version = 99")
            .exec(&mut tx)
            .await?;
        // The version check on a dedicated connection sees the same transaction.
        let rows = sql::query("SELECT version::text FROM agent_schema_version")
            .exec(&mut tx)
            .await?;
        assert!(!rows.is_empty());
        tx.rollback().await?;
    }
    sql::statement("UPDATE agent_schema_version SET version = 99")
        .exec(&mut db)
        .await?;
    assert!(migration::check(&mut db).await.is_err());
    assert!(migration::migrate(&mut db).await.is_err());
    sql::statement("UPDATE agent_schema_version SET version = 1")
        .exec(&mut db)
        .await?;
    sql::statement(
        "INSERT INTO agent_commands VALUES ('migration-test', 'refuse-rollback', 'digest', '{}')",
    )
    .exec(&mut db)
    .await?;
    assert!(migration::rollback_empty(&mut db).await.is_err());
    sql::statement("DELETE FROM agent_commands WHERE scope = 'migration-test'")
        .exec(&mut db)
        .await?;
    migration::rollback_empty(&mut db).await?;
    assert!(migration::check(&mut db).await.is_err());
    migration::migrate(&mut db).await?;
    migration::check(&mut db).await?;
    drop(db);
    common::cleanup(&url).await
}
