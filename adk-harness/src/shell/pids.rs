//! The process groups of background shells, kept so the next start reaps what a crash left
//! running (spec 3.1 §7.29 "Lifetime", §9.2 I70).
//!
//! Each record names a shell's process — the leader of its own process group — and the
//! harness process that owns it, each with the start time the OS reports. A record whose
//! owner no longer runs is an orphan: its group is killed only when the process with its
//! pid has the recorded start time, or no process has that pid any more, so a reused pid is
//! never signalled.

use std::sync::Mutex;

use adk_core::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// One background shell's process, as the registry keeps it.
///
/// # Example
///
/// ```rust
/// use adk_harness::ShellPidRecord;
///
/// let record = ShellPidRecord {
///     pid: 4242,
///     started: "1730000000.000123".into(),
///     owner_pid: 4000,
///     owner_started: "1729999000.000001".into(),
/// };
/// assert_eq!(serde_json::to_value(&record).unwrap()["ownerPid"], 4000);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellPidRecord {
    /// The shell's process id, which is also its process group id.
    pub pid: u32,
    /// The shell's start time as the OS reports it.
    pub started: String,
    /// The harness process that started the shell.
    pub owner_pid: u32,
    /// That process's start time.
    pub owner_started: String,
}

/// Where the harness records the process of each running background shell (spec 3.1
/// §7.29): `harness.db` in adk-cli ([`SqliteShellPidRegistry`]), memory by default.
///
/// # Example
///
/// ```rust
/// use adk_harness::{InMemoryShellPidRegistry, ShellPidRecord, ShellPidRegistry};
///
/// # futures::executor::block_on(async {
/// let registry = InMemoryShellPidRegistry::default();
/// let record = ShellPidRecord { pid: 7, started: "1".into(), owner_pid: 1, owner_started: "0".into() };
/// registry.register(&record).await?;
/// assert_eq!(registry.list().await?, vec![record.clone()]);
/// registry.remove(&record).await?;
/// assert!(registry.list().await?.is_empty());
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[async_trait]
pub trait ShellPidRegistry: Send + Sync {
    /// Records a running shell's process.
    async fn register(&self, record: &ShellPidRecord) -> Result<()>;

    /// Forgets a shell's process once it has ended or been reaped.
    async fn remove(&self, record: &ShellPidRecord) -> Result<()>;

    /// Every recorded process.
    async fn list(&self) -> Result<Vec<ShellPidRecord>>;
}

/// A [`ShellPidRegistry`] in memory: a crash forgets it, so nothing is reaped at the next
/// start.
#[derive(Debug, Default)]
pub struct InMemoryShellPidRegistry {
    records: Mutex<Vec<ShellPidRecord>>,
}

#[async_trait]
impl ShellPidRegistry for InMemoryShellPidRegistry {
    async fn register(&self, record: &ShellPidRecord) -> Result<()> {
        let mut records = self.records.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        records.retain(|existing| existing != record);
        records.push(record.clone());
        Ok(())
    }

    async fn remove(&self, record: &ShellPidRecord) -> Result<()> {
        self.records
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|existing| existing != record);
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ShellPidRecord>> {
        Ok(self.records.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone())
    }
}

/// A [`ShellPidRegistry`] in SQLite, table `adk_harness_shell_pids` (feature `sqlite`).
///
/// It holds process ids and start times only, never a command, so it is not sealed under
/// the store key. Call [`migrate`](Self::migrate) once before use.
///
/// # Example
///
/// ```rust,ignore
/// let pids = adk_harness::SqliteShellPidRegistry::new(pool);
/// pids.migrate().await?;
/// ```
#[cfg(feature = "sqlite")]
#[derive(Debug, Clone)]
pub struct SqliteShellPidRegistry {
    pool: sqlx::SqlitePool,
}

#[cfg(feature = "sqlite")]
impl SqliteShellPidRegistry {
    /// A registry over `pool`.
    pub fn new(pool: sqlx::SqlitePool) -> Self {
        Self { pool }
    }

    /// Creates the registry's table if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns the database error when the statement fails.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS adk_harness_shell_pids (\
                pid INTEGER NOT NULL, started TEXT NOT NULL, owner_pid INTEGER NOT NULL, \
                owner_started TEXT NOT NULL, PRIMARY KEY (pid, started))",
        )
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }
}

#[cfg(feature = "sqlite")]
#[async_trait]
impl ShellPidRegistry for SqliteShellPidRegistry {
    async fn register(&self, record: &ShellPidRecord) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO adk_harness_shell_pids \
             (pid, started, owner_pid, owner_started) VALUES (?, ?, ?, ?)",
        )
        .bind(i64::from(record.pid))
        .bind(&record.started)
        .bind(i64::from(record.owner_pid))
        .bind(&record.owner_started)
        .execute(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    async fn remove(&self, record: &ShellPidRecord) -> Result<()> {
        sqlx::query("DELETE FROM adk_harness_shell_pids WHERE pid = ? AND started = ?")
            .bind(i64::from(record.pid))
            .bind(&record.started)
            .execute(&self.pool)
            .await
            .map_err(crate::fsutil::db_error)?;
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ShellPidRecord>> {
        let rows: Vec<(i64, String, i64, String)> = sqlx::query_as(
            "SELECT pid, started, owner_pid, owner_started FROM adk_harness_shell_pids",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(crate::fsutil::db_error)?;
        Ok(rows
            .into_iter()
            .filter_map(|(pid, started, owner_pid, owner_started)| {
                Some(ShellPidRecord {
                    pid: u32::try_from(pid).ok()?,
                    started,
                    owner_pid: u32::try_from(owner_pid).ok()?,
                    owner_started,
                })
            })
            .collect())
    }
}

/// The start time of process `pid` as the OS reports it, or `None` when no process has
/// that pid or the platform does not say.
pub(crate) fn process_start(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        // SAFETY: `info` is a writable `proc_bsdinfo` of `size` bytes that outlives the call;
        // `proc_pidinfo` writes at most `size` bytes into it.
        let written = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                std::ptr::from_mut(&mut info).cast(),
                size,
            )
        };
        (written == size).then(|| format!("{}.{:06}", info.pbi_start_tvsec, info.pbi_start_tvusec))
    }
    #[cfg(target_os = "linux")]
    {
        // Field 22 of `stat`, the start time in clock ticks since boot; the command name
        // before it is in parentheses and may hold spaces.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after_name = &stat[stat.rfind(')')? + 1..];
        after_name.split_whitespace().nth(19).map(str::to_string)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = pid;
        None
    }
}

/// This process's pid and start time, or `None` where the platform does not report one.
pub(crate) fn this_process() -> Option<(u32, String)> {
    let pid = std::process::id();
    process_start(pid).map(|started| (pid, started))
}

/// Kills the process group of every shell in `registry` whose harness process no longer
/// runs, and forgets it; returns how many groups were signalled (spec 3.1 §7.29).
///
/// A group is signalled only when the process with its pid has the recorded start time, or
/// no process has that pid any more (the leader exited and left its group behind), so a
/// reused pid is never signalled. Records of a live harness process, this one or another,
/// are left alone. A no-op where the OS does not report start times.
///
/// # Errors
///
/// Returns the registry's error.
///
/// # Example
///
/// ```rust
/// use adk_harness::{InMemoryShellPidRegistry, reap_orphaned_shells};
///
/// # tokio::runtime::Runtime::new().unwrap().block_on(async {
/// let registry = InMemoryShellPidRegistry::default();
/// assert_eq!(reap_orphaned_shells(&registry).await?, 0);
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
pub async fn reap_orphaned_shells(registry: &dyn ShellPidRegistry) -> Result<usize> {
    let mut killed = 0;
    for record in registry.list().await? {
        let owner = process_start(record.owner_pid);
        if owner.as_deref() == Some(record.owner_started.as_str()) {
            continue;
        }
        // Without start times nothing can be told apart, so nothing is signalled.
        if this_process().is_none() {
            continue;
        }
        let current = process_start(record.pid);
        if current.is_none() || current.as_deref() == Some(record.started.as_str()) {
            #[cfg(unix)]
            // SAFETY: `killpg` takes a process-group id and a signal, and cannot violate
            // memory safety; a group that no longer exists makes it fail harmlessly.
            let signalled = unsafe { libc::killpg(record.pid as libc::pid_t, libc::SIGKILL) } == 0;
            #[cfg(not(unix))]
            let signalled = false;
            if signalled {
                killed += 1;
                tracing::info!(
                    process.pid = record.pid,
                    "reaped a background shell a crash left running"
                );
            }
        }
        registry.remove(&record).await?;
    }
    Ok(killed)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn this_process_has_a_start_time_that_does_not_change() {
        if cfg!(not(any(target_os = "macos", target_os = "linux"))) {
            return;
        }
        let (pid, started) = this_process().expect("macOS and Linux report a start time");
        assert_eq!(pid, std::process::id());
        assert_eq!(process_start(pid), Some(started));
    }

    /// A record left by a harness process that no longer runs is reaped: the group of a
    /// process with the recorded start time is killed, and one whose pid now names a
    /// process started later is left alone.
    #[tokio::test]
    async fn an_orphan_is_killed_and_a_reused_pid_is_not() {
        if cfg!(not(any(target_os = "macos", target_os = "linux"))) {
            return;
        }
        let spawn = || {
            let mut command = std::process::Command::new("sleep");
            command.arg("30");
            // SAFETY: `setsid` in the forked child is async-signal-safe.
            unsafe {
                std::os::unix::process::CommandExt::pre_exec(&mut command, || {
                    libc::setsid();
                    Ok(())
                });
            }
            command.spawn().unwrap()
        };
        let mut orphan = spawn();
        let mut reused = spawn();
        let registry = InMemoryShellPidRegistry::default();
        // A dead owner: no process has pid 0's start time.
        let dead_owner = |pid: u32, started: String| ShellPidRecord {
            pid,
            started,
            owner_pid: u32::MAX - 1,
            owner_started: "never".into(),
        };
        registry
            .register(&dead_owner(orphan.id(), process_start(orphan.id()).unwrap()))
            .await
            .unwrap();
        registry.register(&dead_owner(reused.id(), "0.000001".into())).await.unwrap();

        assert_eq!(reap_orphaned_shells(&registry).await.unwrap(), 1);
        assert!(registry.list().await.unwrap().is_empty());
        let status = orphan.wait().unwrap();
        assert!(!status.success(), "the orphan was killed: {status:?}");
        assert!(reused.try_wait().unwrap().is_none(), "a reused pid is never signalled");
        reused.kill().unwrap();
        reused.wait().unwrap();
    }

    #[tokio::test]
    async fn a_live_owners_shells_are_left_alone() {
        let Some((pid, started)) = this_process() else { return };
        let registry = InMemoryShellPidRegistry::default();
        let record =
            ShellPidRecord { pid: 1, started: "x".into(), owner_pid: pid, owner_started: started };
        registry.register(&record).await.unwrap();
        assert_eq!(reap_orphaned_shells(&registry).await.unwrap(), 0);
        assert_eq!(registry.list().await.unwrap(), vec![record]);
    }
}
