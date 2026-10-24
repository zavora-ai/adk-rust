//! Persistent sessions and the harness stores in `<config dir>/adk-rust/harness.db`
//! (spec 3.1 §8.1).

use std::path::Path;
use std::sync::Arc;

use adk_session::{ListRequest, SessionService, SqliteSessionService};
use anyhow::{Context, Result};

/// The application name `chat` sessions are kept under.
pub const APP_NAME: &str = "adk-rust";

/// The directory `harness.db` lives in by default: `<config dir>/adk-rust`, which is also
/// the default kernel home (spec 3.2 §7.2).
pub fn default_dir() -> Result<std::path::PathBuf> {
    Ok(dirs::config_dir()
        .context("could not determine the config directory; pass --ephemeral")?
        .join("adk-rust"))
}

/// Opens (and migrates) the session store, creating the file readable by its owner only.
pub async fn open() -> Result<Arc<SqliteSessionService>> {
    open_in(&default_dir()?).await
}

/// [`open`], with `harness.db` in `dir` (`adk-rust up --home`; spec 3.2 §7.2).
///
/// The database runs in write-ahead-log mode, so readers never block the writer and a
/// commit appends to `harness.db-wal` rather than rewriting a rollback journal. Several
/// processes share the file (`adk-rust up`, `chat`, `serve`), and on Windows a
/// rollback-journal commit costs about four times a WAL one: under the kernel's write load
/// writers waited past the 30-second busy timeout.
pub async fn open_in(dir: &Path) -> Result<Arc<SqliteSessionService>> {
    let path = dir.join("harness.db");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    create_private(&path)?;
    let service = SqliteSessionService::new(&sqlite_url(&path)).await.with_context(|| {
        format!(
            "failed to open the session store {}; pass --ephemeral to keep the conversation in memory",
            path.display()
        )
    })?;
    // Persistent in the file. The switch needs the file to itself, so while an older
    // process still has it open in rollback mode, this one keeps that mode.
    match sqlx::query_scalar::<_, String>("PRAGMA journal_mode = WAL")
        .fetch_one(service.pool())
        .await
    {
        Ok(mode) if mode.eq_ignore_ascii_case("wal") => {}
        Ok(mode) => tracing::warn!(journal.mode = %mode, "harness.db stays out of WAL mode"),
        Err(error) => tracing::warn!(error = %error, "harness.db keeps its journal mode for now"),
    }
    service.migrate().await?;
    Ok(Arc::new(service))
}

/// Creates `path` with owner-only permissions when it does not exist yet; conversations
/// can hold anything the person typed.
fn create_private(path: &Path) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    #[cfg(not(windows))]
    let created = options.open(path);
    // A DACL granting the owner alone access stands in for mode 0600 (spec 3.2 §10 W4).
    #[cfg(windows)]
    let created = crate::owner_only::create_new(path);
    match created {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to create {}", path.display())),
    }
}

/// A `sqlite://` URL for `path`. The driver percent-decodes the path and splits off
/// parameters at `?`, so only `%`, `?` and `#` need escaping.
fn sqlite_url(path: &Path) -> String {
    let mut url = String::from("sqlite://");
    for c in path.to_string_lossy().chars() {
        match c {
            '%' => url.push_str("%25"),
            '?' => url.push_str("%3F"),
            '#' => url.push_str("%23"),
            c => url.push(c),
        }
    }
    url.push_str("?mode=rwc");
    url
}

pub use crate::setup::os_user;

/// Every harness store in `harness.db`, sharing one connection pool; each store keeps its
/// own migration table (spec 3.1 §8.1).
#[cfg(feature = "harness")]
pub struct HarnessDb {
    /// The one pool every store writes through; the kernel's stores join it with their own
    /// migration tables (spec 3.2 §7.1).
    pub pool: sqlx::SqlitePool,
    /// Sessions; `EncryptedSession` over SQLite under a store key (spec 3.1 §7.12).
    pub sessions: Arc<dyn SessionService>,
    pub modes: Arc<adk_harness::SqliteModeStore>,
    pub rules: Arc<adk_harness::SqliteStandingRuleStore>,
    pub ledger: Arc<adk_harness::SqliteShellLedger>,
    /// Held calls and decisions; `EncryptedApprovalStore` under a store key.
    pub approvals: Arc<dyn adk_core::ApprovalStore>,
    pub runs: Arc<adk_session::SqliteRunStore>,
    /// Long-term facts (spec 3.1 §7.15.2).
    pub facts: Arc<adk_harness::SqliteFactStore>,
    /// Goals (spec 3.1 §7.11).
    pub goals: Arc<adk_harness::SqliteGoalStore>,
    /// The id of the key the stores are sealed under, if any.
    pub key_id: Option<String>,
    /// Plan and goal checkpoints, sealed under the store key when one is configured
    /// (spec 3.1 §7.11, §7.12, §13 Q5).
    pub plans: Arc<dyn adk_graph::Checkpointer>,
    /// The keyring the stores are sealed under, if any.
    #[cfg(feature = "encrypted")]
    pub keyring: Option<adk_harness::encrypted::StoreKeyring>,
}

/// Opens `harness.db` with the keyring the user config names, if any (spec 3.1 §7.12).
#[cfg(all(feature = "harness", feature = "encrypted"))]
pub async fn open_default_db() -> Result<HarnessDb> {
    let (keyring, _) = crate::store::resolve_keyring(None).await?;
    open_harness_db(keyring.as_ref()).await
}

/// Opens `harness.db` as it is.
#[cfg(all(feature = "harness", not(feature = "encrypted")))]
pub async fn open_default_db() -> Result<HarnessDb> {
    open_harness_db().await
}

/// Opens `harness.db` and migrates every store in it, sealed under `keyring` when one is
/// given (spec 3.1 §7.12, I57): a database sealed under a key refuses to open without it or
/// with another, and one holding plaintext rows refuses a key until `adk-rust store
/// rotate-key` seals them.
#[cfg(all(feature = "harness", feature = "encrypted"))]
pub async fn open_harness_db(
    keyring: Option<&adk_harness::encrypted::StoreKeyring>,
) -> Result<HarnessDb> {
    open_harness_db_in(&default_dir()?, keyring).await
}

/// [`open_harness_db`] in `dir` (`adk-rust up --home`; spec 3.2 §7.2).
#[cfg(all(feature = "harness", feature = "encrypted"))]
pub async fn open_harness_db_in(
    dir: &Path,
    keyring: Option<&adk_harness::encrypted::StoreKeyring>,
) -> Result<HarnessDb> {
    let base = open_in(dir).await?;
    let pool = base.pool().clone();
    let marker = adk_harness::encrypted::SqliteStoreMarker::new(pool.clone());
    marker.migrate().await?;
    marker.check(keyring).await?;
    let modes = adk_harness::SqliteModeStore::new(pool.clone());
    modes.migrate().await?;
    let rules = adk_harness::SqliteStandingRuleStore::new(pool.clone());
    rules.migrate().await?;
    let ledger = adk_harness::SqliteShellLedger::new(pool.clone());
    ledger.migrate().await?;
    let approvals = adk_session::SqliteApprovalStore::from_pool(pool.clone());
    approvals.migrate().await?;
    let runs = adk_session::SqliteRunStore::from_pool(pool.clone());
    runs.migrate().await?;
    let facts = adk_harness::SqliteFactStore::new(pool.clone());
    facts.migrate().await?;
    let goals = adk_harness::SqliteGoalStore::new(pool.clone());
    goals.migrate().await?;
    let plans: Arc<dyn adk_graph::Checkpointer> =
        Arc::new(adk_graph::SqliteCheckpointer::from_pool(pool.clone()).await?);
    let Some(keyring) = keyring else {
        return Ok(HarnessDb {
            pool,
            sessions: base,
            modes: Arc::new(modes),
            rules: Arc::new(rules),
            ledger: Arc::new(ledger),
            approvals: Arc::new(approvals),
            runs: Arc::new(runs),
            facts: Arc::new(facts),
            goals: Arc::new(goals),
            key_id: None,
            plans,
            keyring: None,
        });
    };
    if marker.key_id().await?.is_none() {
        let (rows,): (i64,) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM sessions) + (SELECT count(*) FROM adk_harness_rules) \
             + (SELECT count(*) FROM adk_harness_facts) + (SELECT count(*) FROM adk_approval_pending) \
             + (SELECT count(*) FROM graph_checkpoints)",
        )
        .fetch_one(&pool)
        .await?;
        if rows > 0 {
            anyhow::bail!(
                "harness.db holds {rows} rows written before a store key was configured; run \
                 `adk-rust store rotate-key` to seal them under the key, or drop --store-key"
            );
        }
        marker.set_key_id(&keyring.key_id()).await?;
    }
    let sessions = adk_session::EncryptedSession::new(
        SqliteSessionService::from_pool(pool.clone()),
        keyring.current_key(),
        keyring.previous_keys(),
    );
    let approvals = adk_session::EncryptedApprovalStore::new(
        approvals,
        keyring.current_key(),
        keyring.previous_keys(),
    );
    Ok(HarnessDb {
        pool,
        sessions: Arc::new(sessions),
        modes: Arc::new(modes.with_keyring(keyring.clone())),
        rules: Arc::new(rules.with_keyring(keyring.clone())),
        ledger: Arc::new(ledger.with_keyring(keyring.clone())),
        approvals: Arc::new(approvals),
        runs: Arc::new(runs),
        facts: Arc::new(facts.with_keyring(keyring.clone())),
        goals: Arc::new(goals.with_keyring(keyring.clone())),
        key_id: Some(keyring.key_id()),
        plans: Arc::new(adk_harness::SealedCheckpointer::new(plans, keyring.clone())),
        keyring: Some(keyring.clone()),
    })
}

/// Opens `harness.db` and migrates every store in it.
#[cfg(all(feature = "harness", not(feature = "encrypted")))]
pub async fn open_harness_db() -> Result<HarnessDb> {
    open_harness_db_in(&default_dir()?).await
}

/// [`open_harness_db`] in `dir` (`adk-rust up --home`; spec 3.2 §7.2).
#[cfg(all(feature = "harness", not(feature = "encrypted")))]
pub async fn open_harness_db_in(dir: &Path) -> Result<HarnessDb> {
    let sessions = open_in(dir).await?;
    let pool = sessions.pool().clone();
    let modes = adk_harness::SqliteModeStore::new(pool.clone());
    modes.migrate().await?;
    let rules = adk_harness::SqliteStandingRuleStore::new(pool.clone());
    rules.migrate().await?;
    let ledger = adk_harness::SqliteShellLedger::new(pool.clone());
    ledger.migrate().await?;
    let approvals = adk_session::SqliteApprovalStore::from_pool(pool.clone());
    approvals.migrate().await?;
    let runs = adk_session::SqliteRunStore::from_pool(pool.clone());
    runs.migrate().await?;
    let facts = adk_harness::SqliteFactStore::new(pool.clone());
    facts.migrate().await?;
    let goals = adk_harness::SqliteGoalStore::new(pool.clone());
    goals.migrate().await?;
    let plans = adk_graph::SqliteCheckpointer::from_pool(pool.clone()).await?;
    Ok(HarnessDb {
        pool,
        sessions,
        modes: Arc::new(modes),
        rules: Arc::new(rules),
        ledger: Arc::new(ledger),
        approvals: Arc::new(approvals),
        runs: Arc::new(runs),
        facts: Arc::new(facts),
        goals: Arc::new(goals),
        key_id: None,
        plans: Arc::new(plans),
    })
}

/// The id of `user_id`'s most recently updated session, if any.
pub async fn latest(sessions: &dyn SessionService, user_id: &str) -> Result<Option<String>> {
    let recent = sessions
        .list(ListRequest {
            app_name: APP_NAME.to_string(),
            user_id: user_id.to_string(),
            limit: Some(1),
            offset: None,
        })
        .await?;
    Ok(recent.first().map(|session| session.id().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_session::CreateRequest;
    use std::path::PathBuf;

    #[test]
    fn the_url_keeps_spaces_and_escapes_what_the_driver_would_split_on() {
        assert_eq!(
            sqlite_url(&PathBuf::from("/Users/a b/Application Support/adk-rust/harness.db")),
            "sqlite:///Users/a b/Application Support/adk-rust/harness.db?mode=rwc"
        );
        assert_eq!(
            sqlite_url(&PathBuf::from("/tmp/a?b#c%d.db")),
            "sqlite:///tmp/a%3Fb%23c%25d.db?mode=rwc"
        );
    }

    #[tokio::test]
    async fn sessions_survive_reopening_and_continue_picks_the_latest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dir with space").join("harness.db");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        create_private(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        #[cfg(windows)]
        assert!(crate::owner_only::is_owner_only(&path).unwrap(), "the store is not owner-only");

        let url = sqlite_url(&path);
        let store = SqliteSessionService::new(&url).await.unwrap();
        store.migrate().await.unwrap();
        assert_eq!(latest(&store, "ana").await.unwrap(), None);
        for id in ["first", "second"] {
            store
                .create(CreateRequest {
                    app_name: APP_NAME.into(),
                    user_id: "ana".into(),
                    session_id: Some(id.into()),
                    state: Default::default(),
                })
                .await
                .unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        drop(store);

        let reopened = SqliteSessionService::new(&url).await.unwrap();
        reopened.migrate().await.unwrap();
        assert_eq!(latest(&reopened, "ana").await.unwrap().as_deref(), Some("second"));
        assert_eq!(latest(&reopened, "bo").await.unwrap(), None);
    }

    #[tokio::test]
    async fn harness_db_runs_in_wal_mode_and_stays_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = open_in(dir.path()).await.unwrap();
        let mode: String =
            sqlx::query_scalar("PRAGMA journal_mode").fetch_one(store.pool()).await.unwrap();
        assert_eq!(mode, "wal");
        store
            .create(CreateRequest {
                app_name: APP_NAME.into(),
                user_id: "ana".into(),
                session_id: Some("first".into()),
                state: Default::default(),
            })
            .await
            .unwrap();
        // The log holds the same rows as the database, so it gets the database's mode.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let wal = dir.path().join("harness.db-wal");
            assert_eq!(std::fs::metadata(&wal).unwrap().permissions().mode() & 0o777, 0o600);
        }
        drop(store);
        let reopened = open_in(dir.path()).await.unwrap();
        assert_eq!(latest(reopened.as_ref(), "ana").await.unwrap().as_deref(), Some("first"));
    }
}
