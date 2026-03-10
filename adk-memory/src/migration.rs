//! Lightweight, embedded migration runner for SQL-backed memory services.
//!
//! This module provides shared types and free functions that track applied
//! schema versions in a per-backend registry table and execute only unapplied
//! forward-only migration steps.
//!
//! The types ([`MigrationStep`], [`AppliedMigration`], [`MigrationError`]) are
//! always compiled. The SQL runner functions ([`run_sql_migrations`],
//! [`sql_schema_version`]) require the `sqlite-memory` or `database-memory` feature.

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

/// Generates `run_sql_migrations` and `sql_schema_version` for a concrete
/// sqlx pool type. Each SQL backend (`sqlite-memory`, `database-memory`) gets
/// its own monomorphised copy, avoiding complex generic trait bounds.
#[cfg(any(feature = "sqlite-memory", feature = "database-memory"))]
macro_rules! impl_sql_migration_runner {
    ($mod_name:ident, $pool_ty:ty) => {
        pub mod $mod_name {
            use super::MigrationError;
            use chrono::Utc;
            use sqlx::Row;
            use std::future::Future;

            /// Run all pending migrations for a SQL backend.
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
                // Step 1: Create registry table if missing
                let create_sql = format!(
                    "CREATE TABLE IF NOT EXISTS {registry_table} (\
                        version INTEGER PRIMARY KEY, \
                        description TEXT NOT NULL, \
                        applied_at TEXT NOT NULL\
                    )"
                );
                sqlx::query(&create_sql).execute(pool).await.map_err(|e| {
                    adk_core::AdkError::Memory(format!("migration registry creation failed: {e}"))
                })?;

                // Step 2: Read current max applied version
                let max_sql =
                    format!("SELECT COALESCE(MAX(version), 0) AS max_v FROM {registry_table}");
                let row = sqlx::query(&max_sql).fetch_one(pool).await.map_err(|e| {
                    adk_core::AdkError::Memory(format!("migration registry read failed: {e}"))
                })?;
                let mut max_applied: i64 = row.try_get("max_v").map_err(|e| {
                    adk_core::AdkError::Memory(format!("migration registry read failed: {e}"))
                })?;

                // Step 3: Baseline detection — if registry is empty but
                // tables already exist, record v1 as applied.
                if max_applied == 0 {
                    let existing = detect_existing().await?;
                    if existing {
                        if let Some(&(v, desc, _)) = steps.first() {
                            let now = Utc::now().to_rfc3339();
                            let ins = format!(
                                "INSERT INTO {registry_table} \
                                 (version, description, applied_at) \
                                 VALUES ({v}, '{desc}', '{now}')"
                            );
                            sqlx::query(&ins).execute(pool).await.map_err(|e| {
                                adk_core::AdkError::Memory(format!(
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
                    return Err(adk_core::AdkError::Memory(format!(
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

                    let mut tx = pool.begin().await.map_err(|e| {
                        adk_core::AdkError::Memory(format!(
                            "{}",
                            MigrationError {
                                version,
                                description: description.to_string(),
                                cause: format!("transaction begin failed: {e}"),
                            }
                        ))
                    })?;

                    // Execute the migration SQL (raw_sql supports multiple
                    // semicolon-separated statements in a single call).
                    sqlx::raw_sql(sql).execute(&mut *tx).await.map_err(|e| {
                        adk_core::AdkError::Memory(format!(
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
                         VALUES ({version}, '{description}', '{now}')"
                    );
                    sqlx::query(&rec).execute(&mut *tx).await.map_err(|e| {
                        adk_core::AdkError::Memory(format!(
                            "{}",
                            MigrationError {
                                version,
                                description: description.to_string(),
                                cause: format!("registry record failed: {e}"),
                            }
                        ))
                    })?;

                    tx.commit().await.map_err(|e| {
                        adk_core::AdkError::Memory(format!(
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
            }

            /// Returns the highest applied migration version, or 0 if no
            /// registry table exists or the registry is empty.
            pub async fn sql_schema_version(
                pool: &$pool_ty,
                registry_table: &str,
            ) -> Result<i64, adk_core::AdkError> {
                let sql =
                    format!("SELECT COALESCE(MAX(version), 0) AS max_v FROM {registry_table}");
                match sqlx::query(&sql).fetch_one(pool).await {
                    Ok(row) => {
                        let version: i64 = row.try_get("max_v").unwrap_or(0);
                        Ok(version)
                    }
                    Err(_) => Ok(0),
                }
            }
        }
    };
}

#[cfg(feature = "sqlite-memory")]
impl_sql_migration_runner!(sqlite_runner, sqlx::SqlitePool);

#[cfg(feature = "database-memory")]
impl_sql_migration_runner!(pg_runner, sqlx::PgPool);
