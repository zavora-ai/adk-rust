//! Public migration inputs must remain safe and preserve quoted descriptions.
#![cfg(feature = "sqlite")]
use adk_session::migration::sqlite_runner;
use sqlx::{Connection, SqliteConnection};

const STEPS: &[(i64, &str, &str)] = &[
    (
        1,
        "owner's table",
        "CREATE TABLE widgets (id INTEGER PRIMARY KEY); INSERT INTO widgets VALUES (1)",
    ),
    (2, "widget's name", "ALTER TABLE widgets ADD COLUMN name TEXT"),
];

#[tokio::test]
async fn descriptions_round_trip_on_fresh_and_baselined_databases() {
    for baseline in [false, true] {
        let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        if baseline {
            sqlx::query("CREATE TABLE widgets (id INTEGER PRIMARY KEY)")
                .execute(&mut conn)
                .await
                .unwrap();
        }
        for _ in 0..2 {
            sqlite_runner::run_sql_migrations_on_connection(
                &mut conn,
                "_quoted_migrations",
                STEPS,
                |_| Box::pin(async move { Ok(baseline) }),
            )
            .await
            .unwrap();
        }
        let rows: Vec<(i64, String)> =
            sqlx::query_as("SELECT version, description FROM _quoted_migrations ORDER BY version")
                .fetch_all(&mut conn)
                .await
                .unwrap();
        assert_eq!(rows, vec![(1, "owner's table".to_string()), (2, "widget's name".to_string())]);
    }
}

#[tokio::test]
async fn invalid_identifiers_fail_before_executing_sql() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let too_long = "r".repeat(64);
    for name in [
        "",
        "1registry",
        "registry; DROP TABLE widgets",
        "schema.registry",
        "owner's",
        "é",
        too_long.as_str(),
    ] {
        assert!(
            sqlite_runner::run_sql_migrations(&pool, name, STEPS, || async { Ok(false) })
                .await
                .is_err()
        );
        assert!(sqlite_runner::sql_schema_version(&pool, name).await.is_err());
    }
    let tables: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(tables, Vec::<String>::new());
}

#[tokio::test]
async fn failed_step_rolls_back_its_schema_and_registry_record() {
    let mut conn = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    let steps =
        &[(1, "broken", "CREATE TABLE widgets (id INTEGER); INSERT INTO missing VALUES (1)")];
    assert!(
        sqlite_runner::run_sql_migrations_on_connection(
            &mut conn,
            "_rollback_migrations",
            steps,
            |_| Box::pin(async { Ok(false) }),
        )
        .await
        .is_err()
    );
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name = 'widgets'")
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!(count, 0);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _rollback_migrations")
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn a_63_byte_registry_name_is_accepted() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    let name = "r".repeat(63);
    sqlite_runner::run_sql_migrations(&pool, &name, STEPS, || async { Ok(false) }).await.unwrap();
    assert_eq!(sqlite_runner::sql_schema_version(&pool, &name).await.unwrap(), 2);
}

#[tokio::test]
async fn schema_version_is_zero_only_for_a_missing_registry() {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
    assert_eq!(sqlite_runner::sql_schema_version(&pool, "_absent_migrations").await.unwrap(), 0);

    // A table without the registry's shape is an error, not an unmigrated database.
    sqlx::query("CREATE TABLE _misshapen_migrations (id INTEGER)").execute(&pool).await.unwrap();
    assert!(sqlite_runner::sql_schema_version(&pool, "_misshapen_migrations").await.is_err());

    // So is a database that cannot be reached.
    pool.close().await;
    assert!(sqlite_runner::sql_schema_version(&pool, "_absent_migrations").await.is_err());
}
