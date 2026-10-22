//! Where plan graphs and goal graphs are checkpointed (spec 3.1 §7.11, §13 Q5, feature
//! `graph`).
//!
//! [`checkpointer_from_url`] opens the checkpointer a store URL names, so a deployment
//! checkpoints plans in the backend that holds its sessions. [`SealedCheckpointer`]
//! (feature `encrypted`) seals every checkpoint's payload under the store key
//! (spec 3.1 §7.12, I57).

use std::sync::Arc;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_graph::Checkpointer;

use crate::fsutil::redact_store_url;

fn checkpointer_error(
    code: &'static str,
    category: ErrorCategory,
    message: impl Into<String>,
) -> AdkError {
    AdkError::new(ErrorComponent::Agent, category, code, message)
}

/// The scheme of a store URL, lowercase: `postgres` for `postgres://…`, `sqlite` for
/// `sqlite::memory:`.
fn scheme_of(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once(':')?;
    let valid = !scheme.is_empty()
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then(|| scheme.to_ascii_lowercase())
}

/// The error for a scheme whose backend this build does not compile.
#[cfg(not(all(
    feature = "sqlite",
    feature = "postgres",
    feature = "redis",
    feature = "mongodb",
    feature = "neo4j",
    feature = "firestore"
)))]
fn not_compiled(scheme: &str, feature: &str) -> AdkError {
    checkpointer_error(
        "harness.checkpointer.not_compiled",
        ErrorCategory::InvalidInput,
        format!(
            "a `{scheme}://` checkpointer needs the `{feature}` feature, which this build does \
             not include; rebuild with `--features {feature}` (adk-harness, or adk-cli for \
             `adk-rust`), or name another store"
        ),
    )
}

/// The error for a backend that could not be opened.
#[cfg(any(
    feature = "sqlite",
    feature = "postgres",
    feature = "redis",
    feature = "mongodb",
    feature = "neo4j",
    feature = "firestore"
))]
fn unavailable(url: &str, error: impl std::fmt::Display) -> AdkError {
    checkpointer_error(
        "harness.checkpointer.unavailable",
        ErrorCategory::Unavailable,
        format!("could not open the checkpointer at {}: {error}", redact_store_url(url)),
    )
}

/// The authority and path of `scheme://authority/path?query`.
#[cfg(any(feature = "mongodb", feature = "neo4j", feature = "firestore"))]
fn authority_and_path(url: &str) -> (&str, &str) {
    let rest = url.split_once("://").map_or("", |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    rest.split_once('/').unwrap_or((rest, ""))
}

/// Opens the checkpointer the store URL `url` names and runs its migrations (spec 3.1
/// §7.11, §13 Q5).
///
/// | URL | Checkpointer | Feature |
/// |---|---|---|
/// | `memory:` | `MemoryCheckpointer`: plans do not survive a restart | `graph` |
/// | `sqlite://<path>?mode=rwc` | `SqliteCheckpointer` | `sqlite` |
/// | `postgres://…`, `postgresql://…` | `PostgresCheckpointer` | `postgres` |
/// | `redis://…`, `rediss://…` | `RedisCheckpointer` | `redis` |
/// | `mongodb://…/<database>`, `mongodb+srv://…` (database `adk` when the path is empty) | `MongoCheckpointer` | `mongodb` |
/// | `neo4j://<user>:<password>@<host>`, `neo4j+s://…`, `bolt://…`, `bolt+s://…` | `Neo4jCheckpointer` | `neo4j` |
/// | `firestore://<project>` (honours `FIRESTORE_EMULATOR_HOST`) | `FirestoreCheckpointer` | `firestore` |
///
/// Errors and logs name the URL with its password redacted ([`redact_store_url`]).
///
/// # Example
///
/// ```rust
/// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
/// let checkpointer = adk_harness::checkpointer_from_url("memory:").await?;
/// assert!(checkpointer.load("thread").await.unwrap().is_none());
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
///
/// # Errors
///
/// Returns `harness.checkpointer.url` for a URL with no scheme or an unknown one,
/// `harness.checkpointer.not_compiled` naming the feature a known scheme needs when this
/// build does not include it, and `harness.checkpointer.unavailable` when the backend
/// cannot be opened or migrated.
pub async fn checkpointer_from_url(url: &str) -> Result<Arc<dyn Checkpointer>> {
    let url = url.trim();
    let scheme = scheme_of(url).ok_or_else(|| {
        checkpointer_error(
            "harness.checkpointer.url",
            ErrorCategory::InvalidInput,
            format!(
                "'{}' is not a store URL; use sqlite://, postgres://, redis://, mongodb://, \
                 neo4j://, firestore:// or memory:",
                redact_store_url(url)
            ),
        )
    })?;
    match scheme.as_str() {
        "memory" => Ok(Arc::new(adk_graph::MemoryCheckpointer::new())),
        "sqlite" => sqlite(url).await,
        "postgres" | "postgresql" => postgres(url).await,
        "redis" | "rediss" => redis(url).await,
        "mongodb" | "mongodb+srv" => mongodb(url).await,
        "neo4j" | "neo4j+s" | "neo4j+ssc" | "bolt" | "bolt+s" | "bolt+ssc" => neo4j(url).await,
        "firestore" => firestore(url).await,
        other => Err(checkpointer_error(
            "harness.checkpointer.url",
            ErrorCategory::InvalidInput,
            format!(
                "no checkpointer for `{other}:` URLs; use sqlite://, postgres://, redis://, \
                 mongodb://, neo4j://, firestore:// or memory:"
            ),
        )),
    }
}

#[cfg(feature = "sqlite")]
async fn sqlite(url: &str) -> Result<Arc<dyn Checkpointer>> {
    let checkpointer =
        adk_graph::SqliteCheckpointer::new(url).await.map_err(|error| unavailable(url, error))?;
    Ok(Arc::new(checkpointer))
}

#[cfg(not(feature = "sqlite"))]
async fn sqlite(_url: &str) -> Result<Arc<dyn Checkpointer>> {
    Err(not_compiled("sqlite", "sqlite"))
}

#[cfg(feature = "postgres")]
async fn postgres(url: &str) -> Result<Arc<dyn Checkpointer>> {
    let checkpointer =
        adk_graph::PostgresCheckpointer::new(url).await.map_err(|error| unavailable(url, error))?;
    checkpointer.migrate().await.map_err(|error| unavailable(url, error))?;
    Ok(Arc::new(checkpointer))
}

#[cfg(not(feature = "postgres"))]
async fn postgres(_url: &str) -> Result<Arc<dyn Checkpointer>> {
    Err(not_compiled("postgres", "postgres"))
}

#[cfg(feature = "redis")]
async fn redis(url: &str) -> Result<Arc<dyn Checkpointer>> {
    let checkpointer =
        adk_graph::RedisCheckpointer::new(url).await.map_err(|error| unavailable(url, error))?;
    checkpointer.migrate().await.map_err(|error| unavailable(url, error))?;
    Ok(Arc::new(checkpointer))
}

#[cfg(not(feature = "redis"))]
async fn redis(_url: &str) -> Result<Arc<dyn Checkpointer>> {
    Err(not_compiled("redis", "redis"))
}

#[cfg(feature = "mongodb")]
async fn mongodb(url: &str) -> Result<Arc<dyn Checkpointer>> {
    let (_, path) = authority_and_path(url);
    let database = if path.is_empty() { "adk" } else { path };
    let checkpointer = adk_graph::MongoCheckpointer::new(url, database)
        .await
        .map_err(|error| unavailable(url, error))?;
    checkpointer.migrate().await.map_err(|error| unavailable(url, error))?;
    Ok(Arc::new(checkpointer))
}

#[cfg(not(feature = "mongodb"))]
async fn mongodb(_url: &str) -> Result<Arc<dyn Checkpointer>> {
    Err(not_compiled("mongodb", "mongodb"))
}

#[cfg(feature = "neo4j")]
async fn neo4j(url: &str) -> Result<Arc<dyn Checkpointer>> {
    let (scheme, _) = url.split_once("://").unwrap_or((url, ""));
    let (authority, path) = authority_and_path(url);
    let Some((userinfo, host)) = authority.rsplit_once('@') else {
        return Err(checkpointer_error(
            "harness.checkpointer.url",
            ErrorCategory::InvalidInput,
            format!(
                "a Neo4j checkpointer URL needs credentials: {scheme}://<user>:<password>@{authority}"
            ),
        ));
    };
    let (user, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
    let uri = if path.is_empty() {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}/{path}")
    };
    let checkpointer = adk_graph::Neo4jCheckpointer::new(&uri, user, password)
        .await
        .map_err(|error| unavailable(url, error))?;
    checkpointer.migrate().await.map_err(|error| unavailable(url, error))?;
    Ok(Arc::new(checkpointer))
}

#[cfg(not(feature = "neo4j"))]
async fn neo4j(url: &str) -> Result<Arc<dyn Checkpointer>> {
    let scheme = scheme_of(url).unwrap_or_default();
    Err(not_compiled(&scheme, "neo4j"))
}

#[cfg(feature = "firestore")]
async fn firestore(url: &str) -> Result<Arc<dyn Checkpointer>> {
    let (project, _) = authority_and_path(url);
    if project.is_empty() {
        return Err(checkpointer_error(
            "harness.checkpointer.url",
            ErrorCategory::InvalidInput,
            "a Firestore checkpointer URL names the project: firestore://<project-id>",
        ));
    }
    let checkpointer = adk_graph::FirestoreCheckpointer::new(project)
        .await
        .map_err(|error| unavailable(url, error))?;
    checkpointer.migrate().await.map_err(|error| unavailable(url, error))?;
    Ok(Arc::new(checkpointer))
}

#[cfg(not(feature = "firestore"))]
async fn firestore(_url: &str) -> Result<Arc<dyn Checkpointer>> {
    Err(not_compiled("firestore", "firestore"))
}

#[cfg(feature = "encrypted")]
pub use sealed::SealedCheckpointer;

#[cfg(feature = "encrypted")]
mod sealed {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Arc;

    use adk_core::Result;
    use adk_graph::state::PendingWrite;
    use adk_graph::{Checkpoint, Checkpointer, GraphError, RetentionPolicy, State};
    use async_trait::async_trait;
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    use crate::encrypted::{StoreEnvelope, StoreKeyring};

    /// The state key a sealed checkpoint keeps its envelope under.
    const SEALED_KEY: &str = "__adk_sealed";

    /// The envelope's store name, bound into every checkpoint's associated data.
    const STORE: &str = "checkpoints";

    /// Everything a checkpoint carries besides its identifiers, position and timestamps.
    #[derive(Default, Serialize, Deserialize)]
    #[serde(default)]
    struct Payload {
        state: State,
        metadata: HashMap<String, Value>,
        cleared_interrupt: Option<String>,
        child_ledger: HashMap<String, Value>,
        pending_writes: BTreeMap<String, PendingWrite>,
    }

    fn graph_error(error: adk_core::AdkError) -> GraphError {
        GraphError::CheckpointError(error.message)
    }

    /// A [`Checkpointer`] that seals every checkpoint under the store key (spec 3.1 §7.12,
    /// §9.2 I57, §13 Q5).
    ///
    /// A save seals the checkpoint's payload — its state, metadata, cleared interrupt,
    /// child ledger and pending writes — into one envelope bound to the thread and
    /// checkpoint ids, and stores it as the state's only key. The ids, step, version,
    /// pending node names, attempt counts and creation time stay plaintext, so the inner
    /// checkpointer still orders and fences versions. A read opens the envelope with any
    /// key the keyring holds; a checkpoint in plaintext, or sealed under a key the keyring
    /// does not hold, fails the read, never a plaintext fallback.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    ///
    /// use adk_graph::{Checkpoint, Checkpointer, MemoryCheckpointer};
    /// use adk_harness::SealedCheckpointer;
    /// use adk_harness::encrypted::StoreKeyring;
    ///
    /// # tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
    /// let inner = Arc::new(MemoryCheckpointer::new());
    /// let sealed = SealedCheckpointer::new(inner.clone(), StoreKeyring::generate());
    /// let mut state = adk_graph::State::new();
    /// state.insert("plan".into(), "write the migration".into());
    /// let mut checkpoint = Checkpoint::new("thread", state.clone(), 0, vec![]);
    /// checkpoint.version = 1;
    /// sealed.save(&checkpoint).await?;
    /// assert_eq!(sealed.load("thread").await?.unwrap().state, state);
    /// assert!(!inner.load("thread").await?.unwrap().state.contains_key("plan"));
    /// # Ok::<(), adk_graph::GraphError>(())
    /// # }).unwrap();
    /// ```
    #[derive(Clone)]
    pub struct SealedCheckpointer {
        inner: Arc<dyn Checkpointer>,
        envelope: StoreEnvelope,
    }

    impl std::fmt::Debug for SealedCheckpointer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SealedCheckpointer").finish_non_exhaustive()
        }
    }

    impl SealedCheckpointer {
        /// Seals `inner`'s checkpoints under `keyring`.
        pub fn new(inner: Arc<dyn Checkpointer>, keyring: StoreKeyring) -> Self {
            Self { inner, envelope: StoreEnvelope::new(keyring, STORE) }
        }

        fn seal(&self, checkpoint: &Checkpoint) -> Result<Checkpoint> {
            let payload = Payload {
                state: checkpoint.state.clone(),
                metadata: checkpoint.metadata.clone(),
                cleared_interrupt: checkpoint.cleared_interrupt.clone(),
                child_ledger: checkpoint.child_ledger.clone(),
                pending_writes: checkpoint.pending_writes.clone(),
            };
            let text = serde_json::to_string(&payload)
                .map_err(|error| crate::fsutil::serialize_error("checkpoint", &error))?;
            let sealed = self
                .envelope
                .seal_str(&[&checkpoint.thread_id, &checkpoint.checkpoint_id], &text)?;
            let mut stored = checkpoint.clone();
            stored.state = State::from([(SEALED_KEY.to_string(), Value::String(sealed))]);
            stored.metadata = HashMap::new();
            stored.cleared_interrupt = None;
            stored.child_ledger = HashMap::new();
            stored.pending_writes = BTreeMap::new();
            Ok(stored)
        }

        fn open(&self, mut stored: Checkpoint) -> Result<Checkpoint> {
            let sealed = match stored.state.get(SEALED_KEY) {
                Some(Value::String(sealed)) if stored.state.len() == 1 => sealed.clone(),
                // Not an envelope: the keyring's open refuses it as plaintext.
                _ => serde_json::to_string(&stored.state).unwrap_or_default(),
            };
            let (text, _) =
                self.envelope.open_str(&[&stored.thread_id, &stored.checkpoint_id], &sealed)?;
            let payload: Payload = serde_json::from_str(&text).map_err(|error| {
                adk_core::AdkError::internal(
                    adk_core::ErrorComponent::Agent,
                    "harness.store.decrypt_failed",
                    format!("a sealed checkpoint's payload does not parse: {error}"),
                )
            })?;
            stored.state = payload.state;
            stored.metadata = payload.metadata;
            stored.cleared_interrupt = payload.cleared_interrupt;
            stored.child_ledger = payload.child_ledger;
            stored.pending_writes = payload.pending_writes;
            Ok(stored)
        }

        fn open_all(&self, stored: Vec<Checkpoint>) -> adk_graph::Result<Vec<Checkpoint>> {
            stored
                .into_iter()
                .map(|checkpoint| self.open(checkpoint).map_err(graph_error))
                .collect()
        }

        /// Re-seals the plan checkpoints of the SQLite database behind `pool` under the
        /// current key, seals checkpoints written before a key was configured, and returns
        /// how many rows changed (spec 3.1 §7.12): what `adk-rust store rotate-key` does to
        /// the `graph_checkpoints` table of `harness.db`.
        ///
        /// # Errors
        ///
        /// Returns `harness.store.decrypt_failed` when a row opens under no key the keyring
        /// holds, or the database error.
        #[cfg(feature = "sqlite")]
        pub async fn rewrap_sqlite(&self, pool: &sqlx::SqlitePool) -> Result<u64> {
            type Row = (
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            );
            let present: Option<(String,)> = sqlx::query_as(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'graph_checkpoints'",
            )
            .fetch_optional(pool)
            .await
            .map_err(crate::fsutil::db_error)?;
            if present.is_none() {
                return Ok(0);
            }
            let rows: Vec<Row> = sqlx::query_as(
                "SELECT id, thread_id, state, metadata, cleared_interrupt, child_ledger, \
                 pending_writes FROM graph_checkpoints",
            )
            .fetch_all(pool)
            .await
            .map_err(crate::fsutil::db_error)?;
            let parse = |text: Option<&str>| -> Value {
                text.and_then(|text| serde_json::from_str(text).ok()).unwrap_or(Value::Null)
            };
            let mut changed = 0;
            for (id, thread, state, metadata, cleared, ledger, writes) in rows {
                let address = [thread.as_str(), id.as_str()];
                let state_value = parse(Some(&state));
                let sealed = state_value
                    .as_object()
                    .filter(|object| object.len() == 1)
                    .and_then(|object| object.get(SEALED_KEY))
                    .and_then(Value::as_str);
                let resealed = match sealed {
                    Some(sealed) => self.envelope.reseal(&address, sealed)?,
                    None => {
                        // Columns a checkpoint left empty are absent, so the payload's
                        // defaults apply.
                        let payload: serde_json::Map<String, Value> = [
                            ("state", state_value),
                            ("metadata", parse(metadata.as_deref())),
                            ("cleared_interrupt", cleared.map_or(Value::Null, Value::String)),
                            ("child_ledger", parse(ledger.as_deref())),
                            ("pending_writes", parse(writes.as_deref())),
                        ]
                        .into_iter()
                        .filter(|(_, value)| !value.is_null())
                        .map(|(key, value)| (key.to_string(), value))
                        .collect();
                        let payload: Payload = serde_json::from_value(Value::Object(payload))
                            .map_err(|error| {
                                crate::fsutil::serialize_error("checkpoint", &error)
                            })?;
                        let text = serde_json::to_string(&payload).map_err(|error| {
                            crate::fsutil::serialize_error("checkpoint", &error)
                        })?;
                        Some(self.envelope.seal_str(&address, &text)?)
                    }
                };
                let Some(resealed) = resealed else {
                    continue;
                };
                let marker = serde_json::json!({ SEALED_KEY: resealed }).to_string();
                sqlx::query(
                    "UPDATE graph_checkpoints SET state = ?, metadata = '{}', \
                     cleared_interrupt = NULL, child_ledger = NULL, pending_writes = NULL \
                     WHERE id = ?",
                )
                .bind(marker)
                .bind(&id)
                .execute(pool)
                .await
                .map_err(crate::fsutil::db_error)?;
                changed += 1;
            }
            Ok(changed)
        }
    }

    #[async_trait]
    impl Checkpointer for SealedCheckpointer {
        async fn save(&self, checkpoint: &Checkpoint) -> adk_graph::Result<String> {
            let stored = self.seal(checkpoint).map_err(graph_error)?;
            self.inner.save(&stored).await
        }

        async fn load(&self, thread_id: &str) -> adk_graph::Result<Option<Checkpoint>> {
            self.inner
                .load(thread_id)
                .await?
                .map(|stored| self.open(stored).map_err(graph_error))
                .transpose()
        }

        async fn load_by_id(&self, checkpoint_id: &str) -> adk_graph::Result<Option<Checkpoint>> {
            self.inner
                .load_by_id(checkpoint_id)
                .await?
                .map(|stored| self.open(stored).map_err(graph_error))
                .transpose()
        }

        async fn list(&self, thread_id: &str) -> adk_graph::Result<Vec<Checkpoint>> {
            let stored = self.inner.list(thread_id).await?;
            self.open_all(stored)
        }

        async fn list_page(
            &self,
            thread_id: &str,
            before: Option<u64>,
            limit: usize,
        ) -> adk_graph::Result<Vec<Checkpoint>> {
            let stored = self.inner.list_page(thread_id, before, limit).await?;
            self.open_all(stored)
        }

        async fn delete(&self, thread_id: &str) -> adk_graph::Result<()> {
            self.inner.delete(thread_id).await
        }

        async fn prune(
            &self,
            thread_id: &str,
            policy: &RetentionPolicy,
        ) -> adk_graph::Result<usize> {
            self.inner.prune(thread_id, policy).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes_parse_and_passwords_never_show() {
        assert_eq!(scheme_of("Postgres://h/db").as_deref(), Some("postgres"));
        assert_eq!(scheme_of("sqlite::memory:").as_deref(), Some("sqlite"));
        assert_eq!(scheme_of("mongodb+srv://h").as_deref(), Some("mongodb+srv"));
        assert_eq!(scheme_of("/var/lib/harness.db"), None);
        assert_eq!(
            redact_store_url("neo4j://neo4j:pa:ss@host:7687"),
            "neo4j://neo4j:***@host:7687"
        );
        assert_eq!(redact_store_url("redis://:token@cache:6379/0"), "redis://:***@cache:6379/0");
        assert_eq!(redact_store_url("sqlite:///tmp/a@b.db"), "sqlite:///tmp/a@b.db");
        assert_eq!(redact_store_url("firestore://project"), "firestore://project");
    }

    #[tokio::test]
    async fn unknown_and_malformed_urls_are_refused_without_the_password() {
        for url in ["ftp://user:hunter2@host/x", "harness.db", ""] {
            let error = checkpointer_from_url(url).await.err().expect(url);
            assert_eq!(error.code, "harness.checkpointer.url", "{url}");
            assert!(!error.message.contains("hunter2"), "{}", error.message);
        }
    }
}
