//! Public migration inputs on PostgreSQL: quoted descriptions are bound as values,
//! `sql_schema_version` reports the registry, and a missing registry reads as 0.
//!
//! Each test runs in a fresh schema, which it drops:
//!
//! ```bash
//! DATABASE_URL=postgres://postgres@localhost:5432/postgres \
//!   cargo nextest run -p adk-memory --features database-memory --run-ignored only \
//!   --test sqlx_migration_inputs_postgres
//! ```
#![cfg(feature = "database-memory")]

use std::str::FromStr;

use adk_memory::migration::pg_runner;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, PgPool};

const STEPS: &[(i64, &str, &str)] = &[
    (
        1,
        "owner's \"table\"",
        "CREATE TABLE widgets (id BIGINT PRIMARY KEY); INSERT INTO widgets VALUES (1)",
    ),
    (2, "widget's name -- $$ \\ ;", "ALTER TABLE widgets ADD COLUMN name TEXT"),
];

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres@localhost:5432/postgres".to_string())
}

/// A schema of its own, so the tests run in parallel against one database.
struct Scratch {
    schema: String,
    options: PgConnectOptions,
}

impl Scratch {
    async fn new() -> Self {
        let schema = format!("adk_migration_inputs_{}", uuid::Uuid::new_v4().simple());
        let mut admin = PgConnection::connect(&database_url()).await.expect("connect");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&mut admin)
            .await
            .expect("create schema");
        let options = PgConnectOptions::from_str(&database_url())
            .expect("parse DATABASE_URL")
            .options([("search_path", schema.as_str())]);
        Self { schema, options }
    }

    async fn pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(2)
            .connect_with(self.options.clone())
            .await
            .expect("connect")
    }

    async fn connection(&self) -> PgConnection {
        PgConnection::connect_with(&self.options).await.expect("connect")
    }

    async fn tables(&self) -> Vec<String> {
        let mut admin = PgConnection::connect(&database_url()).await.expect("connect");
        sqlx::query_scalar(
            "SELECT table_name::TEXT FROM information_schema.tables \
             WHERE table_schema = $1 ORDER BY table_name",
        )
        .bind(&self.schema)
        .fetch_all(&mut admin)
        .await
        .expect("list tables")
    }

    async fn drop(self) {
        let mut admin = PgConnection::connect(&database_url()).await.expect("connect");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("DROP SCHEMA {} CASCADE", self.schema)))
            .execute(&mut admin)
            .await
            .expect("drop schema");
    }
}

async fn registry(conn: &mut PgConnection, table: &str) -> Vec<(i64, String)> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT version, description FROM {table} ORDER BY version"
    )))
    .fetch_all(conn)
    .await
    .expect("read registry")
}

fn expected_registry() -> Vec<(i64, String)> {
    STEPS.iter().map(|&(version, description, _)| (version, description.to_string())).collect()
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn descriptions_round_trip_on_fresh_and_baselined_databases() {
    for baseline in [false, true] {
        let scratch = Scratch::new().await;
        let mut conn = scratch.connection().await;
        if baseline {
            sqlx::query("CREATE TABLE widgets (id BIGINT PRIMARY KEY)")
                .execute(&mut conn)
                .await
                .expect("create baseline table");
        }
        for _ in 0..2 {
            pg_runner::run_sql_migrations_on_connection(
                &mut conn,
                "_quoted_migrations",
                STEPS,
                |_| Box::pin(async move { Ok(baseline) }),
            )
            .await
            .expect("migrate");
        }
        assert_eq!(registry(&mut conn, "_quoted_migrations").await, expected_registry());
        conn.close().await.expect("close");
        scratch.drop().await;
    }
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn pool_runs_record_quoted_descriptions_and_report_the_version() {
    let scratch = Scratch::new().await;
    let pool = scratch.pool().await;
    for _ in 0..2 {
        pg_runner::run_sql_migrations(&pool, "_pool_migrations", STEPS, || async { Ok(false) })
            .await
            .expect("migrate");
    }
    assert_eq!(pg_runner::sql_schema_version(&pool, "_pool_migrations").await.expect("version"), 2);
    let mut conn = pool.acquire().await.expect("acquire");
    assert_eq!(registry(&mut conn, "_pool_migrations").await, expected_registry());
    drop(conn);
    pool.close().await;
    scratch.drop().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn invalid_identifiers_fail_before_executing_sql() {
    let scratch = Scratch::new().await;
    let pool = scratch.pool().await;
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
            pg_runner::run_sql_migrations(&pool, name, STEPS, || async { Ok(false) })
                .await
                .is_err(),
            "{name:?} was accepted"
        );
        assert!(pg_runner::sql_schema_version(&pool, name).await.is_err(), "{name:?}");
    }
    pool.close().await;
    assert_eq!(scratch.tables().await, Vec::<String>::new());
    scratch.drop().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn failed_step_rolls_back_its_schema_and_registry_record() {
    let scratch = Scratch::new().await;
    let mut conn = scratch.connection().await;
    let steps =
        &[(1, "broken", "CREATE TABLE widgets (id BIGINT); INSERT INTO missing VALUES (1)")];
    assert!(
        pg_runner::run_sql_migrations_on_connection(
            &mut conn,
            "_rollback_migrations",
            steps,
            |_| { Box::pin(async { Ok(false) }) }
        )
        .await
        .is_err()
    );
    conn.close().await.expect("close");
    assert_eq!(scratch.tables().await, vec!["_rollback_migrations".to_string()]);
    let pool = scratch.pool().await;
    assert_eq!(
        pg_runner::sql_schema_version(&pool, "_rollback_migrations").await.expect("version"),
        0
    );
    pool.close().await;
    scratch.drop().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn a_63_byte_registry_name_is_accepted() {
    let scratch = Scratch::new().await;
    let pool = scratch.pool().await;
    let name = "r".repeat(63);
    pg_runner::run_sql_migrations(&pool, &name, STEPS, || async { Ok(false) })
        .await
        .expect("migrate");
    assert_eq!(pg_runner::sql_schema_version(&pool, &name).await.expect("version"), 2);
    pool.close().await;
    scratch.drop().await;
}

#[tokio::test]
#[ignore = "requires a PostgreSQL server at DATABASE_URL"]
async fn schema_version_is_zero_only_for_a_missing_registry() {
    let scratch = Scratch::new().await;
    let pool = scratch.pool().await;
    assert_eq!(
        pg_runner::sql_schema_version(&pool, "_absent_migrations").await.expect("version"),
        0
    );

    // A table without the registry's shape is an error, not an unmigrated database.
    sqlx::query("CREATE TABLE _misshapen_migrations (id BIGINT)")
        .execute(&pool)
        .await
        .expect("create table");
    assert!(pg_runner::sql_schema_version(&pool, "_misshapen_migrations").await.is_err());

    // So is a database that cannot be reached.
    pool.close().await;
    assert!(pg_runner::sql_schema_version(&pool, "_absent_migrations").await.is_err());
    scratch.drop().await;
}
