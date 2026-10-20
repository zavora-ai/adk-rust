//! Persistent sessions and the harness stores in `<config dir>/adk-rust/harness.db`
//! (spec 3.1 §8.1).

use std::path::Path;
use std::sync::Arc;

use adk_session::{ListRequest, SessionService, SqliteSessionService};
use anyhow::{Context, Result};

/// The application name `chat` sessions are kept under.
pub const APP_NAME: &str = "adk-rust";

/// Opens (and migrates) the session store, creating the file readable by its owner only.
pub async fn open() -> Result<Arc<SqliteSessionService>> {
    let path = dirs::config_dir()
        .context("could not determine the config directory; pass --ephemeral")?
        .join("adk-rust")
        .join("harness.db");
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
    match options.open(path) {
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
    pub sessions: Arc<SqliteSessionService>,
    pub modes: Arc<adk_harness::SqliteModeStore>,
    pub rules: Arc<adk_harness::SqliteStandingRuleStore>,
    pub ledger: Arc<adk_harness::SqliteShellLedger>,
    pub approvals: Arc<adk_session::SqliteApprovalStore>,
    pub runs: Arc<adk_session::SqliteRunStore>,
    /// Long-term facts (spec 3.1 §7.15.2).
    pub facts: Arc<adk_harness::SqliteFactStore>,
    /// Goals (spec 3.1 §7.11).
    pub goals: Arc<adk_harness::SqliteGoalStore>,
}

/// Opens `harness.db` and migrates every store in it.
#[cfg(feature = "harness")]
pub async fn open_harness_db() -> Result<HarnessDb> {
    let sessions = open().await?;
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
    let goals = adk_harness::SqliteGoalStore::new(pool);
    goals.migrate().await?;
    Ok(HarnessDb {
        sessions,
        modes: Arc::new(modes),
        rules: Arc::new(rules),
        ledger: Arc::new(ledger),
        approvals: Arc::new(approvals),
        runs: Arc::new(runs),
        facts: Arc::new(facts),
        goals: Arc::new(goals),
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
}
