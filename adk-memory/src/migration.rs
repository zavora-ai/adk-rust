//! Lightweight, embedded migration runner for SQL-backed memory services.
//!
//! This module provides shared types and free functions that track applied
//! schema versions in a per-backend registry table and execute only unapplied
//! forward-only migration steps.
//!
//! The types ([`MigrationStep`], [`AppliedMigration`], [`MigrationError`]) are
//! always compiled. The SQL runner functions (`run_sql_migrations`,
//! `run_sql_migrations_on_connection`, `sql_schema_version`) require the
//! `sqlite-memory` or `database-memory` feature.

use chrono::{DateTime, Utc};

/// A single forward-only migration step.
///
/// The struct intentionally does not contain the SQL itself — each backend
/// defines its own step list as `&[(i64, &str, &str)]` tuples of
/// `(version, description, sql)`.
#[derive(Debug, Clone, Copy)]
pub struct MigrationStep {
    /// Monotonically increasing version number, starting at 1.
    pub version: i64,
    /// Human-readable description of what this step does.
    pub description: &'static str,
}

/// Record of an applied migration stored in the registry table.
#[derive(Debug, Clone)]
pub struct AppliedMigration {
    /// The applied version number.
    pub version: i64,
    /// Description recorded at apply time.
    pub description: String,
    /// UTC timestamp of application.
    pub applied_at: DateTime<Utc>,
}

/// Error context for a failed migration step.
#[derive(Debug)]
pub struct MigrationError {
    /// The version that failed.
    pub version: i64,
    /// Description of the failed step.
    pub description: String,
    /// Underlying cause (database error message, etc.).
    pub cause: String,
}

impl std::fmt::Display for MigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "migration v{} ({}) failed: {}", self.version, self.description, self.cause)
    }
}

impl std::error::Error for MigrationError {}

// ---------------------------------------------------------------------------
// SQL runner — macro generates concrete implementations per database backend
// ---------------------------------------------------------------------------

// Registry identifiers are interpolated; migration bodies are trusted executable SQL.
// PostgreSQL truncates identifiers past 63 bytes, so a longer name could address
// another registry.
#[cfg(any(feature = "sqlite-memory", feature = "database-memory"))]
fn validate_registry_table(name: &str) -> Result<(), adk_core::AdkError> {
    let mut chars = name.chars();
    if name.len() > 63
        || !chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(adk_core::AdkError::memory(format!(
            "invalid migration registry table `{name}`: use an ASCII identifier of at most 63 bytes \
             starting with a letter or underscore, followed by letters, digits or underscores"
        )));
    }
    Ok(())
}

/// Expands to the body of a migration run, shared by the pool-based and the
/// connection-based entry points.
///
/// `$exec` is expanded at every statement, so `&mut *conn` reborrows a
/// connection afresh for each one.
#[cfg(any(feature = "sqlite-memory", feature = "database-memory"))]
macro_rules! sql_migration_body {
    (
        exec: $exec:expr,
        begin: $begin:expr,
        detect: $detect:expr,
        registry_table: $registry_table:expr,
        steps: $steps:expr,
        int_type: $int_type:expr $(,)?
    ) => {{
        let registry_table: &str = $registry_table;
        super::validate_registry_table(registry_table)?;
        let steps: &[(i64, &str, &str)] = $steps;

        // Step 1: Create registry table if missing
        let create_sql = format!(
            "CREATE TABLE IF NOT EXISTS {registry_table} (\
                version {} PRIMARY KEY, \
                description TEXT NOT NULL, \
                applied_at TEXT NOT NULL\
            )",
            $int_type
        );
        sqlx::query(sqlx::AssertSqlSafe(create_sql)).execute($exec).await.map_err(|e| {
            adk_core::AdkError::memory(format!("migration registry creation failed: {e}"))
        })?;

        // Step 2: Read current max applied version
        let max_sql = format!("SELECT COALESCE(MAX(version), 0) AS max_v FROM {registry_table}");
        let row =
            sqlx::query(sqlx::AssertSqlSafe(max_sql)).fetch_one($exec).await.map_err(|e| {
                adk_core::AdkError::memory(format!("migration registry read failed: {e}"))
            })?;
        let mut max_applied: i64 = row.try_get("max_v").map_err(|e| {
            adk_core::AdkError::memory(format!("migration registry read failed: {e}"))
        })?;

        // Step 3: Baseline detection — if registry is empty but
        // tables already exist, record v1 as applied.
        if max_applied == 0 {
            let existing = $detect.await?;
            if existing {
                if let Some(&(v, desc, _)) = steps.first() {
                    let now = Utc::now().to_rfc3339();
                    let ins = format!(
                        "INSERT INTO {registry_table} \
                         (version, description, applied_at) \
                         VALUES ($1, $2, $3)"
                    );
                    sqlx::query(sqlx::AssertSqlSafe(ins))
                        .bind(v)
                        .bind(desc)
                        .bind(&now)
                        .execute($exec)
                        .await
                        .map_err(|e| {
                            adk_core::AdkError::memory(format!(
                                "{}",
                                MigrationError {
                                    version: v,
                                    description: desc.to_string(),
                                    cause: e.to_string(),
                                }
                            ))
                        })?;
                    max_applied = v;
                }
            }
        }

        // Step 4: Compiled-in max version
        let max_compiled = steps.last().map(|s| s.0).unwrap_or(0);

        // Step 5: Version mismatch check
        if max_applied > max_compiled {
            return Err(adk_core::AdkError::memory(format!(
                "schema version mismatch: database is at v{max_applied} \
                 but code only knows up to v{max_compiled}. \
                 Upgrade your ADK version."
            )));
        }

        // Step 6: Execute unapplied steps in transactions
        for &(version, description, sql) in steps {
            if version <= max_applied {
                continue;
            }

            let mut tx = $begin.await.map_err(|e| {
                adk_core::AdkError::memory(format!(
                    "{}",
                    MigrationError {
                        version,
                        description: description.to_string(),
                        cause: format!("transaction begin failed: {e}"),
                    }
                ))
            })?;

            // Execute the migration SQL (raw_sql supports multiple
            // semicolon-separated statements in a single call). Step bodies may be
            // built at runtime, so the trusted-SQL contract of the runner is asserted.
            sqlx::Executor::execute(&mut *tx, sqlx::raw_sql(sqlx::AssertSqlSafe(sql)))
                .await
                .map_err(|e| {
                    adk_core::AdkError::memory(format!(
                        "{}",
                        MigrationError {
                            version,
                            description: description.to_string(),
                            cause: e.to_string(),
                        }
                    ))
                })?;

            // Record the step in the registry
            let now = Utc::now().to_rfc3339();
            let rec = format!(
                "INSERT INTO {registry_table} \
                 (version, description, applied_at) \
                 VALUES ($1, $2, $3)"
            );
            sqlx::query(sqlx::AssertSqlSafe(rec))
                .bind(version)
                .bind(description)
                .bind(&now)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    adk_core::AdkError::memory(format!(
                        "{}",
                        MigrationError {
                            version,
                            description: description.to_string(),
                            cause: format!("registry record failed: {e}"),
                        }
                    ))
                })?;

            tx.commit().await.map_err(|e| {
                adk_core::AdkError::memory(format!(
                    "{}",
                    MigrationError {
                        version,
                        description: description.to_string(),
                        cause: format!("transaction commit failed: {e}"),
                    }
                ))
            })?;
        }

        Ok(())
    }};
}

/// Generates `run_sql_migrations`, `run_sql_migrations_on_connection`, and
/// `sql_schema_version` for a concrete sqlx pool and connection type. Each SQL
/// backend (`sqlite-memory`, `database-memory`) gets its own monomorphised copy, avoiding complex
/// generic trait bounds.
#[cfg(any(feature = "sqlite-memory", feature = "database-memory"))]
macro_rules! impl_sql_migration_runner {
    ($mod_name:ident, $pool_ty:ty, $conn_ty:ty, $int_type:expr, $missing_table:expr) => {
        /// SQL migration runner for this database backend.
        pub mod $mod_name {
            use super::MigrationError;
            use chrono::Utc;
            use sqlx::{Connection, Row};
            use std::future::Future;
            use std::pin::Pin;

            /// Run all pending migrations for a SQL backend.
            ///
            /// `registry_table` must be an ASCII identifier of at most 63 bytes,
            /// starting with a letter or underscore. Step bodies are trusted executable SQL, never user input.
            /// Descriptions are bound as values and may contain quotes.
            ///
            /// 1. Creates the registry table if it does not exist.
            /// 2. Calls `detect_existing` to check for pre-existing schema
            ///    tables. If tables exist but the registry is empty, records
            ///    version 1 as already applied (baseline).
            /// 3. Reads the maximum applied version from the registry.
            /// 4. If the database version exceeds the compiled-in maximum,
            ///    returns a version-mismatch error.
            /// 5. Executes each unapplied step inside a transaction and
            ///    records it in the registry.
            ///
            /// Each statement takes a connection from `pool`. Use
            /// [`run_sql_migrations_on_connection`] when every statement must
            /// run on the connection that holds a session-level lock.
            pub async fn run_sql_migrations<F, Fut>(
                pool: &$pool_ty,
                registry_table: &str,
                steps: &[(i64, &str, &str)],
                detect_existing: F,
            ) -> Result<(), adk_core::AdkError>
            where
                F: FnOnce() -> Fut,
                Fut: Future<Output = Result<bool, adk_core::AdkError>>,
            {
                sql_migration_body!(
                    exec: pool,
                    begin: pool.begin(),
                    detect: detect_existing(),
                    registry_table: registry_table,
                    steps: steps,
                    int_type: $int_type,
                )
            }

            /// Run all pending migrations on one connection.
            ///
            /// The identifier and trusted-SQL requirements of [`run_sql_migrations`]
            /// apply to this entry point too.
            ///
            /// Performs the same steps as [`run_sql_migrations`], but every
            /// statement, including `detect_existing`, runs on `conn`. A
            /// session-level lock taken on `conn`, such as a PostgreSQL advisory
            /// lock, therefore covers the whole run, and the run needs no second
            /// connection.
            ///
            /// # Errors
            ///
            /// Returns an error when a statement or `detect_existing` fails, or
            /// when the registry records a version newer than the last entry in
            /// `steps`.
            pub async fn run_sql_migrations_on_connection<F>(
                conn: &mut $conn_ty,
                registry_table: &str,
                steps: &[(i64, &str, &str)],
                detect_existing: F,
            ) -> Result<(), adk_core::AdkError>
            where
                F: FnOnce(
                    &mut $conn_ty,
                ) -> Pin<
                    Box<dyn Future<Output = Result<bool, adk_core::AdkError>> + Send + '_>,
                >,
            {
                sql_migration_body!(
                    exec: &mut *conn,
                    begin: conn.begin(),
                    detect: detect_existing(&mut *conn),
                    registry_table: registry_table,
                    steps: steps,
                    int_type: $int_type,
                )
            }

            /// Returns the highest applied migration version, or 0 if no
            /// registry table exists or the registry is empty.
            ///
            /// # Errors
            ///
            /// Returns an error when `registry_table` is not an ASCII identifier of
            /// at most 63 bytes, or when the registry cannot be read for any reason
            /// other than the table being absent, such as a lost connection or a
            /// missing privilege.
            pub async fn sql_schema_version(
                pool: &$pool_ty,
                registry_table: &str,
            ) -> Result<i64, adk_core::AdkError> {
                super::validate_registry_table(registry_table)?;
                let sql =
                    format!("SELECT COALESCE(MAX(version), 0) AS max_v FROM {registry_table}");
                let missing_table: fn(&dyn sqlx::error::DatabaseError) -> bool = $missing_table;
                let row = match sqlx::query(sqlx::AssertSqlSafe(sql)).fetch_one(pool).await {
                    Ok(row) => row,
                    Err(sqlx::Error::Database(error)) if missing_table(error.as_ref()) => {
                        return Ok(0);
                    }
                    Err(e) => {
                        return Err(adk_core::AdkError::memory(format!(
                            "migration registry read failed: {e}"
                        )));
                    }
                };
                row.try_get("max_v").map_err(|e| {
                    adk_core::AdkError::memory(format!("migration registry read failed: {e}"))
                })
            }
        }
    };
}

// SQLite reports a missing table only through its message; PostgreSQL uses
// SQLSTATE 42P01 (undefined_table).
#[cfg(feature = "sqlite-memory")]
impl_sql_migration_runner!(
    sqlite_runner,
    sqlx::SqlitePool,
    sqlx::SqliteConnection,
    "INTEGER",
    |error| error.message().starts_with("no such table")
);

#[cfg(feature = "database-memory")]
impl_sql_migration_runner!(pg_runner, sqlx::PgPool, sqlx::PgConnection, "BIGINT", |error| error
    .code()
    .as_deref()
    == Some("42P01"));
