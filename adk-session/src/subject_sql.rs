//! SQL-backed [`SubjectRegistry`](adk_core::subject::SubjectRegistry) and
//! [`ConsentStore`](adk_core::subject::ConsentStore) implementations.
//!
//! | Table | Key | Holds |
//! |-------|-----|-------|
//! | `adk_subjects` | `subject_key` (`kind:id`) | Pseudonym (unique), state, the link, registration time |
//! | `adk_consent_records` | `id` | One consent record; indexed by `(subject_key, granted_at_us, id)` |
//!
//! A subject key is the subject's kind, a colon, and its id. Kinds contain no colon, so two
//! subjects never share a key.
//!
//! | Change | Statement | Why it has one winner |
//! |--------|-----------|-----------------------|
//! | `ensure`, `set_state` | `INSERT ... ON CONFLICT (subject_key) DO NOTHING` / `DO UPDATE` | The primary key admits one insert |
//! | `link` | `UPDATE ... WHERE subject_key = $k AND linked_to IS NULL AND state = 'active'` | The empty link is compared inside the write |
//! | `grant` | `INSERT ... ON CONFLICT (id) DO NOTHING` | The primary key admits one insert |
//! | `revoke` | `UPDATE ... WHERE id = $k AND revoked_at_us IS NULL` | The first revocation is kept |
//!
//! | Backend | Feature |
//! |---------|---------|
//! | `SqliteSubjectRegistry`, `SqliteConsentStore` | `sqlite` |
//! | `PostgresSubjectRegistry`, `PostgresConsentStore` | `postgres` |

use std::sync::Arc;

use adk_core::principal::SubjectRef;
use adk_core::subject::{
    ConsentEvidence, ConsentPurpose, ConsentRecord, ConsentStore, SubjectRecord, SubjectRegistry,
    SubjectState, consent_conflict_error, erasing_error, link_consumed_error, new_pseudonym,
    validate_link,
};
use adk_core::{Clock, Result, SystemClock};
use chrono::{DateTime, Utc};

use crate::durable_sql::{from_json, from_micros, micros, now, storage_error, to_json};

const CODE: &str = "session.subject_store.storage";

const REGISTRY_TABLE: &str = "_adk_subject_migrations";

const MIGRATIONS: &[(i64, &str, &str)] = &[(
    1,
    "create subject registry and consent tables",
    "\
CREATE TABLE IF NOT EXISTS adk_subjects (\
    subject_key TEXT PRIMARY KEY, \
    subject TEXT NOT NULL, \
    pseudonym TEXT NOT NULL UNIQUE, \
    state TEXT NOT NULL, \
    linked_to TEXT, \
    linked_to_subject TEXT, \
    linked_at_us BIGINT, \
    created_at_us BIGINT NOT NULL\
);\
CREATE INDEX IF NOT EXISTS adk_subjects_linked_to ON adk_subjects (linked_to);\
CREATE TABLE IF NOT EXISTS adk_consent_records (\
    id TEXT PRIMARY KEY, \
    subject_key TEXT NOT NULL, \
    subject TEXT NOT NULL, \
    purpose TEXT NOT NULL, \
    policy_version TEXT NOT NULL, \
    granted_at_us BIGINT NOT NULL, \
    expires_at_us BIGINT, \
    revoked_at_us BIGINT, \
    evidence TEXT\
);\
CREATE INDEX IF NOT EXISTS adk_consent_records_subject \
    ON adk_consent_records (subject_key, granted_at_us, id);",
)];

const GET_SUBJECT: &str = "\
SELECT subject, pseudonym, state, linked_to_subject, linked_at_us, created_at_us \
FROM adk_subjects WHERE subject_key = $1";

// Parameters appear in ascending order in every statement, so SQLite, which numbers
// `$N` parameters by first appearance, binds them as PostgreSQL does.
const ENSURE: &str = "\
INSERT INTO adk_subjects (subject_key, subject, pseudonym, state, created_at_us) \
VALUES ($1, $2, $3, 'active', $4) \
ON CONFLICT (subject_key) DO NOTHING";

const SET_STATE: &str = "\
INSERT INTO adk_subjects (subject_key, subject, pseudonym, state, created_at_us) \
VALUES ($1, $2, $3, $4, $5) \
ON CONFLICT (subject_key) DO UPDATE SET state = excluded.state";

const LINK: &str = "\
UPDATE adk_subjects SET linked_to = $1, linked_to_subject = $2, linked_at_us = $3 \
WHERE subject_key = $4 AND linked_to IS NULL AND state = 'active'";

const REMOVE: &str = "DELETE FROM adk_subjects WHERE subject_key = $1";

const GRANT: &str = "\
INSERT INTO adk_consent_records \
    (id, subject_key, subject, purpose, policy_version, granted_at_us, expires_at_us, \
     revoked_at_us, evidence) \
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
ON CONFLICT (id) DO NOTHING";

const GET_CONSENT: &str = "\
SELECT id, subject, purpose, policy_version, granted_at_us, expires_at_us, revoked_at_us, \
    evidence \
FROM adk_consent_records WHERE id = $1";

const REVOKE: &str = "\
UPDATE adk_consent_records SET revoked_at_us = $1 WHERE id = $2 AND revoked_at_us IS NULL";

const ERASE: &str = "DELETE FROM adk_consent_records WHERE subject_key = $1";

/// The key a subject is stored under: `kind:id`. Kinds contain no colon.
fn subject_key(subject: &SubjectRef) -> String {
    subject.to_string()
}

fn state_name(state: SubjectState) -> Result<String> {
    match serde_json::to_value(state) {
        Ok(serde_json::Value::String(name)) => Ok(name),
        Ok(other) => Err(storage_error(CODE, format!("subject state encodes as {other}"))),
        Err(e) => Err(storage_error(CODE, format!("subject state does not encode: {e}"))),
    }
}

type SubjectRow = (String, String, String, Option<String>, Option<i64>, i64);

fn decode_subject(
    (subject, pseudonym, state, linked_to, linked_at_us, created_at_us): SubjectRow,
) -> Result<SubjectRecord> {
    Ok(SubjectRecord {
        subject: from_json(CODE, "subject", &subject)?,
        pseudonym,
        state: serde_json::from_value(serde_json::Value::String(state))
            .map_err(|e| storage_error(CODE, format!("column 'state' is corrupt: {e}")))?,
        linked_to: linked_to.map(|json| from_json(CODE, "linked_to_subject", &json)).transpose()?,
        linked_at: linked_at_us.map(|us| from_micros(CODE, "linked_at_us", us)).transpose()?,
        created_at: from_micros(CODE, "created_at_us", created_at_us)?,
    })
}

type ConsentRow = (String, String, String, String, i64, Option<i64>, Option<i64>, Option<String>);

fn decode_consent(
    (id, subject, purpose, policy_version, granted, expires, revoked, evidence): ConsentRow,
) -> Result<ConsentRecord> {
    let instant = |column: &str, us: Option<i64>| -> Result<Option<DateTime<Utc>>> {
        us.map(|us| from_micros(CODE, column, us)).transpose()
    };
    Ok(ConsentRecord {
        id,
        subject: from_json(CODE, "subject", &subject)?,
        purpose: ConsentPurpose::from(purpose),
        policy_version,
        granted_at: from_micros(CODE, "granted_at_us", granted)?,
        expires_at: instant("expires_at_us", expires)?,
        revoked_at: instant("revoked_at_us", revoked)?,
        evidence: evidence
            .map(|json| from_json::<ConsentEvidence>(CODE, "evidence", &json))
            .transpose()?,
    })
}

fn failed(e: sqlx::Error) -> adk_core::AdkError {
    storage_error(CODE, format!("subject store query failed: {e}"))
}

macro_rules! sql_subject_stores {
    (
        $(#[$registry_doc:meta])*
        $registry:ident,
        $(#[$consent_doc:meta])*
        $consent:ident,
        $pool:ty, $collate:literal, $migrate:expr
    ) => {
        $(#[$registry_doc])*
        #[derive(Debug, Clone)]
        pub struct $registry {
            pool: $pool,
            clock: Arc<dyn Clock>,
        }

        impl $registry {
            /// Connects to `database_url` and creates a connection pool.
            ///
            /// Call [`migrate`](Self::migrate) before first use.
            ///
            /// # Errors
            ///
            /// Returns `session.subject_store.storage` when the database cannot be reached.
            pub async fn new(database_url: &str) -> Result<Self> {
                let pool = <$pool>::connect(database_url).await.map_err(|e| {
                    storage_error(CODE, format!("subject store connection failed: {e}"))
                })?;
                Ok(Self::from_pool(pool))
            }

            /// Uses an existing pool, such as the one the session service holds.
            pub fn from_pool(pool: $pool) -> Self {
                Self { pool, clock: Arc::new(SystemClock) }
            }

            /// Sets the clock that stamps registration and link times.
            #[must_use]
            pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
                self.clock = clock;
                self
            }

            /// Creates or upgrades the registry and consent tables.
            ///
            /// # Errors
            ///
            /// Returns an error when a migration statement fails.
            pub async fn migrate(&self) -> Result<()> {
                let migrate = $migrate;
                migrate(&self.pool).await
            }

            async fn insert_if_absent(&self, subject: &SubjectRef) -> Result<()> {
                let (_, created_at_us) = now(&*self.clock);
                sqlx::query(ENSURE)
                    .bind(subject_key(subject))
                    .bind(to_json(CODE, subject)?)
                    .bind(new_pseudonym())
                    .bind(created_at_us)
                    .execute(&self.pool)
                    .await
                    .map_err(failed)?;
                Ok(())
            }

            async fn required(&self, subject: &SubjectRef) -> Result<SubjectRecord> {
                self.get(subject).await?.ok_or_else(|| {
                    storage_error(CODE, format!("{} was written but does not read back", subject.kind))
                })
            }
        }

        #[async_trait::async_trait]
        impl SubjectRegistry for $registry {
            async fn get(&self, subject: &SubjectRef) -> Result<Option<SubjectRecord>> {
                let row: Option<SubjectRow> = sqlx::query_as(GET_SUBJECT)
                .bind(subject_key(subject))
                .fetch_optional(&self.pool)
                .await
                .map_err(failed)?;
                row.map(decode_subject).transpose()
            }

            async fn ensure(&self, subject: &SubjectRef) -> Result<SubjectRecord> {
                self.insert_if_absent(subject).await?;
                self.required(subject).await
            }

            async fn by_pseudonym(&self, pseudonym: &str) -> Result<Option<SubjectRef>> {
                let subject: Option<String> =
                    sqlx::query_scalar("SELECT subject FROM adk_subjects WHERE pseudonym = $1")
                        .bind(pseudonym)
                        .fetch_optional(&self.pool)
                        .await
                        .map_err(failed)?;
                subject.map(|json| from_json(CODE, "subject", &json)).transpose()
            }

            async fn link(
                &self,
                visitor: &SubjectRef,
                identified: &SubjectRef,
            ) -> Result<SubjectRecord> {
                validate_link(visitor, identified)?;
                for subject in [visitor, identified] {
                    if self.get(subject).await?.is_some_and(|r| r.state == SubjectState::Erasing) {
                        return Err(erasing_error(subject));
                    }
                }
                if self.get(visitor).await?.is_some_and(|record| record.linked_to.is_some()) {
                    return Err(link_consumed_error());
                }
                self.insert_if_absent(identified).await?;
                self.insert_if_absent(visitor).await?;
                let (_, linked_at_us) = now(&*self.clock);
                let changed = sqlx::query(LINK)
                    .bind(subject_key(identified))
                    .bind(to_json(CODE, identified)?)
                    .bind(linked_at_us)
                    .bind(subject_key(visitor))
                    .execute(&self.pool)
                    .await
                    .map_err(failed)?;
                let record = self.required(visitor).await?;
                if changed.rows_affected() == 0 {
                    return Err(if record.state == SubjectState::Erasing {
                        erasing_error(visitor)
                    } else {
                        link_consumed_error()
                    });
                }
                Ok(record)
            }

            async fn linked(&self, identified: &SubjectRef) -> Result<Vec<SubjectRef>> {
                let rows: Vec<String> = sqlx::query_scalar(concat!(
                    "SELECT subject FROM adk_subjects WHERE linked_to = $1 ORDER BY subject_key",
                    $collate
                ))
                .bind(subject_key(identified))
                .fetch_all(&self.pool)
                .await
                .map_err(failed)?;
                rows.iter().map(|json| from_json(CODE, "subject", json)).collect()
            }

            async fn set_state(
                &self,
                subject: &SubjectRef,
                state: SubjectState,
            ) -> Result<SubjectRecord> {
                let (_, created_at_us) = now(&*self.clock);
                sqlx::query(SET_STATE)
                    .bind(subject_key(subject))
                    .bind(to_json(CODE, subject)?)
                    .bind(new_pseudonym())
                    .bind(state_name(state)?)
                    .bind(created_at_us)
                    .execute(&self.pool)
                    .await
                    .map_err(failed)?;
                self.required(subject).await
            }

            async fn remove(&self, subject: &SubjectRef) -> Result<bool> {
                let removed = sqlx::query(REMOVE)
                    .bind(subject_key(subject))
                    .execute(&self.pool)
                    .await
                    .map_err(failed)?;
                Ok(removed.rows_affected() > 0)
            }
        }

        $(#[$consent_doc])*
        #[derive(Debug, Clone)]
        pub struct $consent {
            pool: $pool,
        }

        impl $consent {
            /// Connects to `database_url` and creates a connection pool.
            ///
            /// Call [`migrate`](Self::migrate) before first use.
            ///
            /// # Errors
            ///
            /// Returns `session.subject_store.storage` when the database cannot be reached.
            pub async fn new(database_url: &str) -> Result<Self> {
                let pool = <$pool>::connect(database_url).await.map_err(|e| {
                    storage_error(CODE, format!("consent store connection failed: {e}"))
                })?;
                Ok(Self::from_pool(pool))
            }

            /// Uses an existing pool, such as the one the session service holds.
            pub fn from_pool(pool: $pool) -> Self {
                Self { pool }
            }

            /// Creates or upgrades the registry and consent tables.
            ///
            /// # Errors
            ///
            /// Returns an error when a migration statement fails.
            pub async fn migrate(&self) -> Result<()> {
                let migrate = $migrate;
                migrate(&self.pool).await
            }
        }

        #[async_trait::async_trait]
        impl ConsentStore for $consent {
            async fn grant(&self, record: ConsentRecord) -> Result<()> {
                let stamp = |at: Option<DateTime<Utc>>| at.map(|at| micros(at).1);
                let inserted = sqlx::query(GRANT)
                    .bind(&record.id)
                    .bind(subject_key(&record.subject))
                    .bind(to_json(CODE, &record.subject)?)
                    .bind(record.purpose.as_str())
                    .bind(&record.policy_version)
                    .bind(micros(record.granted_at).1)
                    .bind(stamp(record.expires_at))
                    .bind(stamp(record.revoked_at))
                    .bind(record.evidence.as_ref().map(|e| to_json(CODE, e)).transpose()?)
                    .execute(&self.pool)
                    .await
                    .map_err(failed)?;
                if inserted.rows_affected() == 0 {
                    return Err(consent_conflict_error(&record.id));
                }
                Ok(())
            }

            async fn get(&self, id: &str) -> Result<Option<ConsentRecord>> {
                let row: Option<ConsentRow> = sqlx::query_as(GET_CONSENT)
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(failed)?;
                row.map(decode_consent).transpose()
            }

            async fn for_subject(&self, subject: &SubjectRef) -> Result<Vec<ConsentRecord>> {
                let rows: Vec<ConsentRow> = sqlx::query_as(concat!(
                    "SELECT id, subject, purpose, policy_version, granted_at_us, expires_at_us, \
                     revoked_at_us, evidence FROM adk_consent_records WHERE subject_key = $1 \
                     ORDER BY granted_at_us, id",
                    $collate
                ))
                .bind(subject_key(subject))
                .fetch_all(&self.pool)
                .await
                .map_err(failed)?;
                rows.into_iter().map(decode_consent).collect()
            }

            async fn revoke(&self, id: &str, at: DateTime<Utc>) -> Result<Option<ConsentRecord>> {
                sqlx::query(REVOKE)
                    .bind(micros(at).1)
                    .bind(id)
                    .execute(&self.pool)
                    .await
                    .map_err(failed)?;
                self.get(id).await
            }

            async fn erase(&self, subject: &SubjectRef) -> Result<u64> {
                let erased = sqlx::query(ERASE)
                    .bind(subject_key(subject))
                    .execute(&self.pool)
                    .await
                    .map_err(failed)?;
                Ok(erased.rows_affected())
            }
        }
    };
}

#[cfg(feature = "sqlite")]
sql_subject_stores! {
    /// SQLite-backed [`SubjectRegistry`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_core::principal::SubjectRef;
    /// use adk_core::subject::SubjectRegistry;
    /// use adk_session::SqliteSubjectRegistry;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let registry = SqliteSubjectRegistry::new("sqlite://subjects.db?mode=rwc").await?;
    /// registry.migrate().await?;
    /// let record = registry.ensure(&SubjectRef::human("alice")).await?;
    /// assert!(record.pseudonym.starts_with("psn:"));
    /// # Ok(())
    /// # }
    /// ```
    SqliteSubjectRegistry,
    /// SQLite-backed [`ConsentStore`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_core::subject::{ConsentPurpose, ConsentRecord, ConsentStore, VisitorId};
    /// use adk_session::SqliteConsentStore;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let consent = SqliteConsentStore::new("sqlite://subjects.db?mode=rwc").await?;
    /// consent.migrate().await?;
    /// let visitor = VisitorId::generate().subject();
    /// consent.grant(ConsentRecord::new(visitor, ConsentPurpose::Analytics, "2026-10")).await?;
    /// # Ok(())
    /// # }
    /// ```
    SqliteConsentStore,
    sqlx::SqlitePool,
    "",
    |pool: &sqlx::SqlitePool| {
        let pool = pool.clone();
        async move {
            crate::migration::sqlite_runner::run_sql_migrations(
                &pool,
                REGISTRY_TABLE,
                MIGRATIONS,
                || async { Ok(false) },
            )
            .await
        }
    }
}

#[cfg(feature = "postgres")]
sql_subject_stores! {
    /// PostgreSQL-backed [`SubjectRegistry`] shared by every replica.
    /// [`migrate`](Self::migrate) takes an advisory lock, so concurrent processes can call it
    /// at startup.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_core::principal::SubjectRef;
    /// use adk_core::subject::SubjectRegistry;
    /// use adk_session::PostgresSubjectRegistry;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let registry = PostgresSubjectRegistry::new("postgres://localhost/app").await?;
    /// registry.migrate().await?;
    /// let record = registry.ensure(&SubjectRef::human("alice")).await?;
    /// # let _ = record;
    /// # Ok(())
    /// # }
    /// ```
    PostgresSubjectRegistry,
    /// PostgreSQL-backed [`ConsentStore`] shared by every replica.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use adk_core::subject::{ConsentPurpose, ConsentRecord, ConsentStore, VisitorId};
    /// use adk_session::PostgresConsentStore;
    ///
    /// # async fn example() -> adk_core::Result<()> {
    /// let consent = PostgresConsentStore::new("postgres://localhost/app").await?;
    /// consent.migrate().await?;
    /// let visitor = VisitorId::generate().subject();
    /// consent.grant(ConsentRecord::new(visitor, ConsentPurpose::Analytics, "2026-10")).await?;
    /// # Ok(())
    /// # }
    /// ```
    PostgresConsentStore,
    sqlx::PgPool,
    // Byte order, as Rust compares strings.
    " COLLATE \"C\"",
    |pool: &sqlx::PgPool| {
        let pool = pool.clone();
        async move {
            crate::migration::run_pg_migrations_locked(&pool, REGISTRY_TABLE, MIGRATIONS).await
        }
    }
}
