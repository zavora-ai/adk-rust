//! The control-plane client the CLI and other surfaces use (spec 3.2 §7.11, §8.1).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use adk_core::{AdkError, ApprovalAction, ErrorCategory, ErrorComponent};
use futures::stream::{self, BoxStream, StreamExt};
use hyper::body::Bytes;
use hyper::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::api::{
    AgendaItemResponse, AgendaQuery, AgendaResponse, ApprovalItem, AttachRequest, AttachResponse,
    FreezeRequest, FreezeResponse, KillRequest, NiceRequest, ProcessResponse, ProcessesQuery,
    ProcessesResponse, RejectRequest, ScheduleRequest, SpawnRequest,
};
use super::{ControlPlaneConfig, paths};
use crate::agenda::AgendaItem;
use crate::bus::EventRecord;
use crate::error::{self, KernelError, Result};
use crate::kernel::KernelHealth;
use crate::process::Process;
use crate::sched::QueueClass;
use crate::wake::WakeSet;

/// The approvals inbox, beside the kernel routes under `/api`.
const APPROVALS: &str = "/approvals";

/// How long a call other than [`events`](ControlClient::events) waits for the kernel.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// A client of a running kernel's control plane (spec 3.2 §7.11), over its Unix socket, its
/// named pipe on Windows (§10 W2) or HTTP. Surfaces hold no state (K-I17): every call reads
/// the kernel.
///
/// # Example
///
/// ```rust,no_run
/// use adk_kernel::control::ControlClient;
///
/// # async fn example(home: &std::path::Path) -> adk_kernel::Result<()> {
/// let client = match ControlClient::connect_local(home).await? {
///     Some(client) => client,
///     None => ControlClient::http("https://kernel.example.com").with_bearer_token("token"),
/// };
/// for process in client.processes(&Default::default()).await? {
///     println!("{} {:?}", process.meta.pid, process.run.state);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ControlClient {
    transport: Transport,
    token: Option<String>,
}

#[derive(Debug, Clone)]
enum Transport {
    Socket(PathBuf),
    #[cfg(windows)]
    Pipe(String),
    Http {
        base: String,
        client: reqwest::Client,
    },
}

/// A response's status and body.
struct Answer {
    status: StatusCode,
    body: BoxStream<'static, Result<Bytes>>,
}

impl Answer {
    async fn bytes(self) -> Result<Vec<u8>> {
        let mut body = self.body;
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next().await {
            bytes.extend_from_slice(&chunk?);
        }
        Ok(bytes)
    }
}

impl ControlClient {
    /// A client of the kernel listening on the Unix socket at `path`.
    pub fn socket(path: impl Into<PathBuf>) -> Self {
        Self { transport: Transport::Socket(path.into()), token: None }
    }

    /// A client of the kernel listening on the named pipe `name` (Windows; spec 3.2 §10 W2),
    /// as [`ControlPlaneConfig::pipe_name`] names it. The client talks to the pipe only when
    /// its owner is the user running the client.
    #[cfg(windows)]
    pub fn pipe(name: impl Into<String>) -> Self {
        Self { transport: Transport::Pipe(name.into()), token: None }
    }

    /// A client of the kernel serving HTTP at `base_url`, the routes under its `/api`.
    pub fn http(base_url: impl Into<String>) -> Self {
        let base = base_url.into().trim_end_matches('/').to_string();
        Self { transport: Transport::Http { base, client: reqwest::Client::new() }, token: None }
    }

    /// Sends `token` as a bearer token.
    #[must_use]
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// The bearer token the client sends, so a caller of other routes on the same control
    /// plane (the approvals inbox, spec 3.3 §6) sends the same one.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::control::ControlClient;
    ///
    /// let client = ControlClient::http("https://kernel.example.com").with_bearer_token("token");
    /// assert_eq!(client.bearer_token(), Some("token"));
    /// assert_eq!(ControlClient::socket("/run/kernel.sock").bearer_token(), None);
    /// ```
    pub fn bearer_token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Where the client connects: `unix:<path>`, `pipe:<name>` or the base URL.
    pub fn endpoint(&self) -> String {
        match &self.transport {
            Transport::Socket(path) => format!("unix:{}", path.display()),
            #[cfg(windows)]
            Transport::Pipe(name) => format!("pipe:{name}"),
            Transport::Http { base, .. } => base.clone(),
        }
    }

    /// A client of the kernel running at `home`, or `None` when none answers on its socket,
    /// or on Windows its named pipe (spec 3.2 §10 W2).
    ///
    /// A socket left by a kernel that did not stop cleanly counts as none; a pipe goes away
    /// with its kernel.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the socket or pipe exists but this user may
    /// not use it: another account owns it, or its permissions refuse this user.
    pub async fn connect_local(home: &Path) -> Result<Option<Self>> {
        #[cfg(windows)]
        {
            let Some(name) = ControlPlaneConfig::default().pipe_name(home) else {
                return Ok(None);
            };
            match super::pipe::answers(&name).await {
                Ok(true) => Ok(Some(Self::pipe(name))),
                Ok(false) => Ok(None),
                Err(error) => Err(KernelError::ControlPlane {
                    reason: format!(
                        "the kernel pipe {name} exists but cannot be used: {error}; it admits \
                         only the OS user that runs the kernel"
                    ),
                }),
            }
        }
        #[cfg(not(windows))]
        {
            let Some(path) = ControlPlaneConfig::default().socket_path(home) else {
                return Ok(None);
            };
            connect_local_socket(path).await
        }
    }

    /// `GET /kernel/health`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn health(&self) -> Result<KernelHealth> {
        self.call(Method::GET, paths::HEALTH.to_string(), None::<&()>).await
    }

    /// `GET /kernel/partitions`: the partition table (spec 3.2 §7.8).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn partitions(&self) -> Result<Vec<crate::partition::Partition>> {
        let answer: super::api::PartitionsResponse =
            self.call(Method::GET, paths::PARTITIONS.to_string(), None::<&()>).await?;
        Ok(answer.partitions)
    }

    /// `POST /kernel/partitions`: creates or replaces `partition` (spec 3.2 §7.8).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn put_partition(
        &self,
        partition: &crate::partition::Partition,
    ) -> Result<crate::partition::Partition> {
        let answer: super::api::PartitionResponse =
            self.call(Method::POST, paths::PARTITIONS.to_string(), Some(partition)).await?;
        Ok(answer.partition)
    }

    /// `POST /kernel/partitions/{id}/mode`: sets partition `id`'s mode (spec 3.2 §8.1).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn set_partition_mode(
        &self,
        id: &str,
        mode: crate::partition::PartitionMode,
    ) -> Result<crate::partition::Partition> {
        let path = paths::PARTITION_MODE.replace("{id}", &encode(id));
        let body = super::api::PartitionModeRequest { mode };
        let answer: super::api::PartitionResponse =
            self.call(Method::POST, path, Some(&body)).await?;
        Ok(answer.partition)
    }

    /// `GET /kernel/settings`: every area's quiet time, daily spending limit and mode in
    /// force (spec 3.3 §10 K3).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `403` without the `operator` scope or a device's `read`.
    pub async fn settings(&self) -> Result<Vec<super::api::AreaView>> {
        let answer: super::api::SettingsResponse =
            self.call(Method::GET, paths::SETTINGS.to_string(), None::<&()>).await?;
        Ok(answer.areas)
    }

    /// `GET /kernel/settings/{partition}`: one area's settings in force (spec 3.3 §10 K3).
    ///
    /// # Errors
    ///
    /// As [`settings`](Self::settings), and `404` for an unknown area.
    pub async fn area_settings(&self, partition: &str) -> Result<super::api::AreaView> {
        let path = paths::AREA_SETTINGS.replace("{partition}", &encode(partition));
        let answer: super::api::AreaResponse = self.call(Method::GET, path, None::<&()>).await?;
        Ok(answer.area)
    }

    /// `POST /kernel/settings/{partition}/quiet-time`: sets the area's quiet time, or turns
    /// it off with `None` (spec 3.3 §10 K3).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `403` for a device token or without the `operator` scope,
    /// `404` for an unknown area.
    pub async fn set_quiet_time(
        &self,
        partition: &str,
        window: Option<crate::sched::QuietHours>,
    ) -> Result<super::api::AreaView> {
        let path = paths::QUIET_TIME.replace("{partition}", &encode(partition));
        let body = super::api::QuietTimeRequest { window };
        let answer: super::api::AreaResponse = self.call(Method::POST, path, Some(&body)).await?;
        Ok(answer.area)
    }

    /// `POST /kernel/settings/{partition}/spending-limit`: sets the area's daily spending
    /// limit in micro-USD, or removes it with `None` (spec 3.3 §10 K3).
    ///
    /// # Errors
    ///
    /// As [`set_quiet_time`](Self::set_quiet_time).
    pub async fn set_spending_limit(
        &self,
        partition: &str,
        micro_usd: Option<u64>,
    ) -> Result<super::api::AreaView> {
        let path = paths::SPENDING_LIMIT.replace("{partition}", &encode(partition));
        let body = super::api::SpendingLimitRequest { micro_usd };
        let answer: super::api::AreaResponse = self.call(Method::POST, path, Some(&body)).await?;
        Ok(answer.area)
    }

    /// `POST /kernel/settings/{partition}/mode`: sets how much the area's agents act alone
    /// (spec 3.3 §10 K3).
    ///
    /// # Errors
    ///
    /// As [`set_quiet_time`](Self::set_quiet_time).
    pub async fn set_area_mode(
        &self,
        partition: &str,
        mode: crate::partition::PartitionMode,
    ) -> Result<super::api::AreaView> {
        let path = paths::AREA_MODE.replace("{partition}", &encode(partition));
        let body = super::api::PartitionModeRequest { mode };
        let answer: super::api::AreaResponse = self.call(Method::POST, path, Some(&body)).await?;
        Ok(answer.area)
    }

    /// `GET /kernel/away`: "While you were away" after the viewer's cursor (spec 3.3 §10
    /// K4). A paired device reads as itself; `query.viewer` names any other surface.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    #[cfg(feature = "proc")]
    pub async fn away(&self, query: &super::api::AwayQuery) -> Result<super::api::AwayResponse> {
        let mut pairs = Vec::new();
        if let Some(viewer) = &query.viewer {
            pairs.push(format!("viewer={}", encode(viewer)));
        }
        if let Some(after) = query.after {
            pairs.push(format!("after={after}"));
        }
        if let Some(limit) = query.limit {
            pairs.push(format!("limit={limit}"));
        }
        let path = match pairs.is_empty() {
            true => paths::AWAY.to_string(),
            false => format!("{}?{}", paths::AWAY, pairs.join("&")),
        };
        self.call(Method::GET, path, None::<&()>).await
    }

    /// `POST /kernel/away/seen`: marks "While you were away" seen through row `through`, a
    /// page's `through` (spec 3.3 §10 K4); returns the cursor, which never moves back.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `400` for a row the ledger does not hold yet.
    #[cfg(feature = "proc")]
    pub async fn mark_away_seen(
        &self,
        viewer: Option<&str>,
        through: u64,
    ) -> Result<super::AwayCursor> {
        let body = super::api::SeenRequest { viewer: viewer.map(str::to_string), through };
        let answer: super::api::SeenResponse =
            self.call(Method::POST, paths::AWAY_SEEN.to_string(), Some(&body)).await?;
        Ok(answer.cursor)
    }

    /// `GET /kernel/away/lines/{seq}`: one line of "While you were away" with its ledger
    /// row, the line's record (spec 3.3 §10 K4).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `404` for a row the ledger does not hold.
    #[cfg(feature = "proc")]
    pub async fn away_line(&self, seq: u64) -> Result<super::api::AwayLineResponse> {
        let path = paths::AWAY_LINE.replace("{seq}", &seq.to_string());
        self.call(Method::GET, path, None::<&()>).await
    }

    /// `GET /kernel/memory`: what the agents remember about the caller, with provenance, and
    /// the overnight tidy-ups (spec 3.3 §10 K5).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `400` when the kernel shows no memory.
    pub async fn memory(&self, area: Option<&str>) -> Result<super::api::MemoryResponse> {
        let path = match area {
            Some(area) => format!("{}?area={}", paths::MEMORY, encode(area)),
            None => paths::MEMORY.to_string(),
        };
        self.call(Method::GET, path, None::<&()>).await
    }

    /// `POST /kernel/memory/facts/{id}/confirm`: "That's right" (spec 3.3 §8.2, §10 K5).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `403` for a device token, `404` for a fact that is not
    /// the caller's.
    pub async fn confirm_fact(&self, id: &str) -> Result<super::RememberedFact> {
        let path = paths::FACT_CONFIRM.replace("{id}", &encode(id));
        let answer: super::api::FactResponse = self.call(Method::POST, path, Some(&())).await?;
        Ok(answer.fact)
    }

    /// `POST /kernel/memory/facts/{id}/fix`: "Fix it" with the person's `text` (spec 3.3
    /// §8.2, §10 K5).
    ///
    /// # Errors
    ///
    /// As [`confirm_fact`](Self::confirm_fact), and `400` for a text the harness refuses.
    pub async fn fix_fact(&self, id: &str, text: &str) -> Result<super::RememberedFact> {
        let path = paths::FACT_FIX.replace("{id}", &encode(id));
        let body = super::api::FixRequest { text: text.to_string() };
        let answer: super::api::FactResponse = self.call(Method::POST, path, Some(&body)).await?;
        Ok(answer.fact)
    }

    /// `POST /kernel/memory/facts/{id}/forget`: "Forget it" (spec 3.3 §8.2, §10 K5).
    ///
    /// # Errors
    ///
    /// As [`confirm_fact`](Self::confirm_fact).
    pub async fn forget_fact(&self, id: &str) -> Result<()> {
        let path = paths::FACT_FORGET.replace("{id}", &encode(id));
        let _: super::api::ForgetResponse = self.call(Method::POST, path, Some(&())).await?;
        Ok(())
    }

    /// `POST /kernel/memory/undo`: "Undo" the tidy-up `id`, or the latest one that can be
    /// undone (spec 3.3 §8.2, §10 K5).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `403` for a device token, `404` when nothing is left to
    /// undo, `409` for a tidy-up unfinished or undone.
    pub async fn undo_tidy_up(&self, id: Option<&str>) -> Result<super::TidyUpUndo> {
        let body = super::api::UndoRequest { tidy_up: id.map(str::to_string) };
        self.call(Method::POST, paths::MEMORY_UNDO.to_string(), Some(&body)).await
    }

    /// `GET /kernel/record`: the latest `limit` actions on record, newest first (spec 3.3
    /// §10 K5).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `400` when the kernel shows no record.
    pub async fn record(&self, limit: Option<usize>) -> Result<Vec<super::RecordLine>> {
        let path = match limit {
            Some(limit) => format!("{}?limit={limit}", paths::RECORD),
            None => paths::RECORD.to_string(),
        };
        let answer: super::api::RecordResponse = self.call(Method::GET, path, None::<&()>).await?;
        Ok(answer.lines)
    }

    /// `GET /kernel/record/check`: whether every action is on record and nothing was
    /// changed or deleted, as `adk-rust audit verify` answers (spec 3.3 §10 K5).
    ///
    /// # Errors
    ///
    /// As [`record`](Self::record).
    pub async fn check_record(&self) -> Result<super::RecordCheck> {
        self.call(Method::GET, paths::RECORD_CHECK.to_string(), None::<&()>).await
    }

    /// `GET /kernel/processes`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn processes(&self, query: &ProcessesQuery) -> Result<Vec<Process>> {
        let mut pairs = Vec::new();
        if let Some(partition) = &query.partition {
            pairs.push(format!("partition={}", encode(partition.as_str())));
        }
        if let Some(state) = query.state {
            let state = serde_json::to_value(state)?;
            pairs.push(format!("state={}", encode(state.as_str().unwrap_or_default())));
        }
        if let Some(limit) = query.limit {
            pairs.push(format!("limit={limit}"));
        }
        let path = match pairs.is_empty() {
            true => paths::PROCESSES.to_string(),
            false => format!("{}?{}", paths::PROCESSES, pairs.join("&")),
        };
        let answer: ProcessesResponse = self.call(Method::GET, path, None::<&()>).await?;
        Ok(answer.processes)
    }

    /// `POST /kernel/processes`: starts a process of a registered target as the caller.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `404` for an unknown target, `403` without the `operator`
    /// scope.
    pub async fn spawn(&self, request: &SpawnRequest) -> Result<Process> {
        let answer: ProcessResponse =
            self.call(Method::POST, paths::PROCESSES.to_string(), Some(request)).await?;
        Ok(answer.process)
    }

    /// `GET /approvals`: the held calls the caller may decide, pending first page.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `404` when it serves no approvals inbox.
    pub async fn approvals(&self) -> Result<Vec<ApprovalItem>> {
        #[derive(serde::Deserialize)]
        struct Page {
            items: Vec<ApprovalItem>,
        }
        let page: Page =
            self.call(Method::GET, format!("{APPROVALS}?limit=200"), None::<&()>).await?;
        Ok(page.items)
    }

    /// `POST /approvals/{id}/decide`: decides a held call, at the `version` the caller read.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `409` for a stale version, `403` for a caller who may not
    /// decide it.
    pub async fn decide(
        &self,
        id: &str,
        version: u64,
        action: ApprovalAction,
        reason: Option<&str>,
    ) -> Result<ApprovalItem> {
        let body = serde_json::json!({
            "expectedVersion": version,
            "action": action,
            "reason": reason,
        });
        let path = format!("{APPROVALS}/{}/decide", encode(id));
        self.call(Method::POST, path, Some(&body)).await
    }

    /// `POST /kernel/processes/{pid}/kill`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn kill(&self, pid: &str, reason: Option<&str>) -> Result<Process> {
        let body = KillRequest { reason: reason.map(str::to_string) };
        self.process_action(paths::KILL, pid, &body).await
    }

    /// `POST /kernel/processes/{pid}/nice`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn nice(&self, pid: &str, queue: QueueClass) -> Result<Process> {
        self.process_action(paths::NICE, pid, &NiceRequest { queue }).await
    }

    /// `POST /kernel/processes/{pid}/suspend`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn suspend(&self, pid: &str) -> Result<Process> {
        self.process_action(paths::SUSPEND, pid, &()).await
    }

    /// `POST /kernel/processes/{pid}/resume`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn resume(&self, pid: &str) -> Result<Process> {
        self.process_action(paths::RESUME, pid, &()).await
    }

    /// `GET /kernel/drivers`: the mounted drivers (spec 3.2 §7.7).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    #[cfg(feature = "drivers")]
    pub async fn drivers(&self) -> Result<Vec<crate::drivers::DriverView>> {
        let answer: super::api::DriversResponse =
            self.call(Method::GET, paths::DRIVERS.to_string(), None::<&()>).await?;
        Ok(answer.drivers)
    }

    /// `POST /kernel/drivers/{id}/mount`: what mounting `manifest` (its TOML) approves,
    /// and with `confirm` the mount itself.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    #[cfg(feature = "drivers")]
    pub async fn mount_driver(
        &self,
        id: &str,
        manifest: &str,
        confirm: bool,
    ) -> Result<super::api::MountDriverResponse> {
        let mut body = super::api::MountDriverRequest::new(manifest);
        body.confirm = confirm;
        let path = paths::DRIVER_MOUNT.replace("{id}", &encode(id));
        self.call(Method::POST, path, Some(&body)).await
    }

    /// `POST /kernel/drivers/{id}/unmount`; `false` when no such driver was mounted.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    #[cfg(feature = "drivers")]
    pub async fn unmount_driver(&self, id: &str) -> Result<bool> {
        let path = paths::DRIVER_UNMOUNT.replace("{id}", &encode(id));
        let answer: super::api::UnmountDriverResponse =
            self.call(Method::POST, path, Some(&())).await?;
        Ok(answer.unmounted)
    }

    /// `POST /kernel/drivers/{id}/restart`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    #[cfg(feature = "drivers")]
    pub async fn restart_driver(&self, id: &str) -> Result<crate::drivers::DriverView> {
        let path = paths::DRIVER_RESTART.replace("{id}", &encode(id));
        let answer: super::api::DriverResponse = self.call(Method::POST, path, Some(&())).await?;
        Ok(answer.driver)
    }

    /// `POST /kernel/drivers/{id}/reapprove`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    #[cfg(feature = "drivers")]
    pub async fn reapprove_driver(&self, id: &str) -> Result<crate::drivers::DriverView> {
        let path = paths::DRIVER_REAPPROVE.replace("{id}", &encode(id));
        let answer: super::api::DriverResponse = self.call(Method::POST, path, Some(&())).await?;
        Ok(answer.driver)
    }

    /// `POST /kernel/freeze`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn freeze(&self, reason: Option<&str>) -> Result<FreezeResponse> {
        let body = FreezeRequest { reason: reason.map(str::to_string) };
        self.call(Method::POST, paths::FREEZE.to_string(), Some(&body)).await
    }

    /// `POST /kernel/unfreeze`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn unfreeze(&self) -> Result<FreezeResponse> {
        self.call(Method::POST, paths::UNFREEZE.to_string(), Some(&())).await
    }

    /// `GET /kernel/proc/ledger` as JSON: one page of the activity ledger (spec 3.2 §7.10).
    /// Page on with `params.after` set to the answer's `next`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `403` without the `operator` scope.
    #[cfg(feature = "proc")]
    pub async fn ledger(
        &self,
        params: &super::api::LedgerParams,
    ) -> Result<super::api::LedgerResponse> {
        let mut pairs = Vec::new();
        let mut pair = |key: &str, value: Option<String>| {
            if let Some(value) = value {
                pairs.push(format!("{key}={}", encode(&value)));
            }
        };
        pair("partition", params.partition.as_ref().map(|p| p.as_str().to_string()));
        pair("kind", params.kind.clone());
        pair("agent", params.agent.clone());
        pair("from", params.from.map(|from| from.to_rfc3339()));
        pair("to", params.to.map(|to| to.to_rfc3339()));
        pair("after", params.after.map(|after| after.to_string()));
        pair("limit", params.limit.map(|limit| limit.to_string()));
        let path = match pairs.is_empty() {
            true => paths::PROC_LEDGER.to_string(),
            false => format!("{}?{}", paths::PROC_LEDGER, pairs.join("&")),
        };
        self.call(Method::GET, path, None::<&()>).await
    }

    /// `GET /kernel/proc/baselines`: each partition's last baseline pass (spec 3.2 §7.10).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    #[cfg(feature = "proc")]
    pub async fn baselines(&self) -> Result<Vec<crate::proc::PassState>> {
        let answer: super::api::BaselinesResponse =
            self.call(Method::GET, paths::PROC_BASELINES.to_string(), None::<&()>).await?;
        Ok(answer.partitions)
    }

    /// `GET /kernel/proc/observations`: the observations the kernel offered, newest first
    /// (spec 3.2 §7.10).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    #[cfg(feature = "proc")]
    pub async fn observations(
        &self,
        params: &super::api::ObservationsParams,
    ) -> Result<Vec<crate::proc::Observation>> {
        let mut pairs = Vec::new();
        if let Some(partition) = &params.partition {
            pairs.push(format!("partition={}", encode(partition.as_str())));
        }
        if let Some(since) = params.since {
            pairs.push(format!("since={since}"));
        }
        if let Some(limit) = params.limit {
            pairs.push(format!("limit={limit}"));
        }
        let path = match pairs.is_empty() {
            true => paths::PROC_OBSERVATIONS.to_string(),
            false => format!("{}?{}", paths::PROC_OBSERVATIONS, pairs.join("&")),
        };
        let answer: super::api::ObservationsResponse =
            self.call(Method::GET, path, None::<&()>).await?;
        Ok(answer.observations)
    }

    /// `GET /kernel/events`: the event bus from just after `after`, then live.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached; a broken
    /// stream ends with the error as its last item.
    pub async fn events(
        &self,
        after: Option<u64>,
    ) -> Result<BoxStream<'static, Result<EventRecord>>> {
        let path = match after {
            Some(cursor) => format!("{}?after={cursor}", paths::EVENTS),
            None => paths::EVENTS.to_string(),
        };
        let answer = self.send(Method::GET, &path, None, None).await?;
        if !answer.status.is_success() {
            let status = answer.status;
            return Err(answer_error(status, &answer.bytes().await?));
        }
        Ok(server_sent_records(answer.body))
    }

    /// `POST /kernel/attach/{session}`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn attach(
        &self,
        session_id: &str,
        request: &AttachRequest,
    ) -> Result<AttachResponse> {
        let path = paths::ATTACH.replace("{session}", &encode(session_id));
        self.call(Method::POST, path, Some(request)).await
    }

    /// `GET /kernel/agenda` (spec 3.2 §7.5, §8.1 `adk-rust agenda list`).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn agenda(&self, query: &AgendaQuery) -> Result<Vec<AgendaItem>> {
        let mut pairs = Vec::new();
        if let Some(partition) = &query.partition {
            pairs.push(format!("partition={}", encode(partition.as_str())));
        }
        if let Some(state) = query.state {
            pairs.push(format!("state={}", encode(state.as_str())));
        }
        if let Some(limit) = query.limit {
            pairs.push(format!("limit={limit}"));
        }
        let path = match pairs.is_empty() {
            true => paths::AGENDA.to_string(),
            false => format!("{}?{}", paths::AGENDA, pairs.join("&")),
        };
        let answer: AgendaResponse = self.call(Method::GET, path, None::<&()>).await?;
        Ok(answer.items)
    }

    /// `POST /kernel/agenda/{id}/approve`: approves the item as the caller, then schedules it.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `404` for an unknown item, `409` for one not proposed.
    pub async fn approve_item(&self, id: &str) -> Result<AgendaItem> {
        self.agenda_action(paths::AGENDA_APPROVE, id, &()).await
    }

    /// `POST /kernel/agenda/{id}/reject`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn reject_item(&self, id: &str, reason: Option<&str>) -> Result<AgendaItem> {
        let body = RejectRequest { reason: reason.map(str::to_string) };
        self.agenda_action(paths::AGENDA_REJECT, id, &body).await
    }

    /// `POST /kernel/agenda/{id}/schedule`: puts the approved item's process to sleep on
    /// `wake`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn schedule_item(&self, id: &str, wake: WakeSet) -> Result<AgendaItem> {
        self.agenda_action(paths::AGENDA_SCHEDULE, id, &ScheduleRequest { wake }).await
    }

    /// `POST /kernel/pairing`: issues a pairing code for a device acting for the caller
    /// (spec 3.3 §10 K2, `adk-rust pair`).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `403` without the `operator` scope, `400` when the kernel
    /// pairs no devices.
    pub async fn issue_pairing_code(
        &self,
        request: &super::api::IssueRequest,
    ) -> Result<crate::pairing::IssuedCode> {
        self.call(Method::POST, paths::PAIRING.to_string(), Some(request)).await
    }

    /// `POST /kernel/pairing/claim`: pairs this client's device with `code` and returns its
    /// token, once (spec 3.3 §10 K2). The call needs no token and sends none, so a client
    /// whose old token was revoked or expired re-pairs; send the one it returns with
    /// [`with_bearer_token`](Self::with_bearer_token) from then on.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `401` for a wrong, used or expired code.
    pub async fn claim_pairing(
        &self,
        code: &str,
        name: Option<&str>,
    ) -> Result<crate::pairing::ClaimedDevice> {
        let body =
            super::api::ClaimRequest { code: code.to_string(), name: name.map(str::to_string) };
        let anonymous = Self { transport: self.transport.clone(), token: None };
        anonymous.call(Method::POST, paths::PAIRING_CLAIM.to_string(), Some(&body)).await
    }

    /// `GET /kernel/devices`: every paired device (spec 3.3 §10 K2, `adk-rust pair list`).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn devices(&self) -> Result<Vec<crate::pairing::DeviceView>> {
        let answer: super::api::DevicesResponse =
            self.call(Method::GET, paths::DEVICES.to_string(), None::<&()>).await?;
        Ok(answer.devices)
    }

    /// `POST /kernel/devices/{id}/revoke`: removes a device; its token stops working at once
    /// (spec 3.3 §10 K2, `adk-rust pair revoke`).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `404` for an unknown device.
    pub async fn revoke_device(&self, id: &str) -> Result<crate::pairing::DeviceView> {
        let path = paths::DEVICE_REVOKE.replace("{id}", &encode(id));
        let answer: super::api::DeviceResponse = self.call(Method::POST, path, Some(&())).await?;
        Ok(answer.device)
    }

    /// `GET /kernel/channels`: the paired chat accounts and the ones waiting for their code
    /// to be accepted (spec 3.3 §10 K1).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered; a kernel without chat channels answers none.
    pub async fn channel_accounts(&self) -> Result<super::api::ChannelAccountsResponse> {
        self.call(Method::GET, paths::CHANNELS.to_string(), None::<&()>).await
    }

    /// `POST /kernel/channels/pair`: accepts the code a chat account was sent, so its
    /// messages reach the caller's agents (spec 3.3 §10 K1).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `401` for a wrong or expired code, `400` when the kernel
    /// has no chat channels.
    pub async fn accept_channel(&self, code: &str) -> Result<super::api::ChannelAccountView> {
        let body = super::api::AcceptChannelRequest { code: code.to_string() };
        let answer: super::api::ChannelAccountResponse =
            self.call(Method::POST, paths::CHANNEL_PAIR.to_string(), Some(&body)).await?;
        Ok(answer.account)
    }

    /// `POST /kernel/channels/{id}/revoke`: removes a chat account (spec 3.3 §10 K1).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `404` for an unknown account.
    pub async fn revoke_channel_account(&self, id: &str) -> Result<super::api::ChannelAccountView> {
        let path = paths::CHANNEL_REVOKE.replace("{id}", &encode(id));
        let answer: super::api::ChannelAccountResponse =
            self.call(Method::POST, path, Some(&())).await?;
        Ok(answer.account)
    }

    /// `POST /kernel/channels/waiting/{id}/dismiss`: dismisses a chat account waiting to be
    /// paired; its code stops working (spec 3.3 §10 K1).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `404` for an account that is not waiting.
    pub async fn dismiss_channel_request(
        &self,
        id: &str,
    ) -> Result<super::api::ChannelPairingView> {
        let path = paths::CHANNEL_DISMISS.replace("{id}", &encode(id));
        let answer: super::api::ChannelPairingResponse =
            self.call(Method::POST, path, Some(&())).await?;
        Ok(answer.waiting)
    }

    async fn agenda_action(
        &self,
        route: &str,
        id: &str,
        body: &impl Serialize,
    ) -> Result<AgendaItem> {
        let path = route.replace("{id}", &encode(id));
        let answer: AgendaItemResponse = self.call(Method::POST, path, Some(body)).await?;
        Ok(answer.item)
    }

    async fn process_action(
        &self,
        route: &str,
        pid: &str,
        body: &impl Serialize,
    ) -> Result<Process> {
        let path = route.replace("{pid}", &encode(pid));
        let answer: ProcessResponse = self.call(Method::POST, path, Some(body)).await?;
        Ok(answer.process)
    }

    /// Sends a JSON call and decodes the JSON answer, or the error the kernel answered.
    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: String,
        body: Option<&impl Serialize>,
    ) -> Result<T> {
        let body = body.map(serde_json::to_vec).transpose()?;
        let answer = self.send(method, &path, body, Some(CALL_TIMEOUT)).await?;
        let status = answer.status;
        let bytes = answer.bytes().await?;
        if !status.is_success() {
            return Err(answer_error(status, &bytes));
        }
        serde_json::from_slice(&bytes).map_err(|error| KernelError::ControlPlane {
            reason: format!(
                "the kernel at {} answered {path} with a body this client cannot read ({error}); \
                 are the CLI and the kernel the same version?",
                self.endpoint()
            ),
        })
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Option<Duration>,
    ) -> Result<Answer> {
        let path = format!("{}{path}", paths::API_PREFIX);
        let unreachable = |error: &dyn std::fmt::Display| KernelError::ControlPlane {
            reason: format!(
                "could not reach the kernel at {}: {error}; start it with `adk-rust up`",
                self.endpoint()
            ),
        };
        match &self.transport {
            Transport::Http { base, client } => {
                let mut request = client.request(method, format!("{base}{path}"));
                if let Some(token) = &self.token {
                    request = request.bearer_auth(token);
                }
                if let Some(body) = body {
                    request = request.header("content-type", "application/json").body(body);
                }
                if let Some(timeout) = timeout {
                    request = request.timeout(timeout);
                }
                let response = request.send().await.map_err(|error| unreachable(&error))?;
                let status = StatusCode::from_u16(response.status().as_u16())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let body = response
                    .bytes_stream()
                    .map(|chunk| {
                        chunk.map_err(|error| KernelError::ControlPlane {
                            reason: format!("the kernel's answer broke off: {error}"),
                        })
                    })
                    .boxed();
                Ok(Answer { status, body })
            }
            local => {
                let send = local_request(local, method, &path, self.token.as_deref(), body);
                let answer = match timeout {
                    Some(timeout) => tokio::time::timeout(timeout, send)
                        .await
                        .map_err(|_| unreachable(&"the kernel did not answer in time"))?,
                    None => send.await,
                };
                answer.map_err(|error| unreachable(&error))
            }
        }
    }
}

#[cfg(unix)]
async fn connect_local_socket(path: PathBuf) -> Result<Option<ControlClient>> {
    match tokio::fs::symlink_metadata(&path).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(KernelError::ControlPlane {
                reason: format!("could not read the kernel socket {}: {error}", path.display()),
            });
        }
    }
    match tokio::net::UnixStream::connect(&path).await {
        Ok(_) => Ok(Some(ControlClient::socket(path))),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(KernelError::ControlPlane {
            reason: format!(
                "the kernel socket {} exists but cannot be opened: {error}; it admits only the \
                 OS user that runs the kernel",
                path.display()
            ),
        }),
    }
}

#[cfg(all(not(unix), not(windows)))]
async fn connect_local_socket(_path: PathBuf) -> Result<Option<ControlClient>> {
    Ok(None)
}

/// One HTTP/1.1 exchange over the socket at `socket`.
#[cfg(unix)]
async fn socket_exchange(
    socket: &Path,
    request: hyper::Request<http_body_util::Full<Bytes>>,
) -> std::result::Result<hyper::Response<hyper::body::Incoming>, String> {
    let stream =
        tokio::net::UnixStream::connect(socket).await.map_err(|error| error.to_string())?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .map_err(|error| error.to_string())?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::debug!(error = %error, "kernel socket connection closed");
        }
    });
    sender.send_request(request).await.map_err(|error| error.to_string())
}

/// One HTTP/1.1 exchange over the local transport `local`: the socket, or the pipe.
async fn local_request(
    local: &Transport,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Vec<u8>>,
) -> std::result::Result<Answer, String> {
    use http_body_util::{BodyStream, Full};

    let mut request =
        hyper::Request::builder().method(method).uri(path).header(hyper::header::HOST, "localhost");
    if let Some(token) = token {
        request = request.header(hyper::header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if body.is_some() {
        request = request.header(hyper::header::CONTENT_TYPE, "application/json");
    }
    let request = request
        .body(Full::new(Bytes::from(body.unwrap_or_default())))
        .map_err(|error| error.to_string())?;
    let response = match local {
        #[cfg(unix)]
        Transport::Socket(socket) => socket_exchange(socket, request).await?,
        #[cfg(not(unix))]
        Transport::Socket(socket) => {
            let _ = request;
            return Err(format!(
                "the local control socket {} needs a Unix platform; on Windows the kernel \
                 serves a named pipe, which ControlClient::connect_local finds",
                socket.display()
            ));
        }
        #[cfg(windows)]
        Transport::Pipe(name) => super::pipe::request(name, request).await?,
        Transport::Http { base, .. } => {
            return Err(format!("{base} is reached over HTTP, not over a local transport"));
        }
    };
    let status = response.status();
    let body = BodyStream::new(response.into_body())
        .filter_map(|frame| async move {
            match frame {
                Ok(frame) => frame.into_data().ok().map(Ok),
                Err(error) => Some(Err(KernelError::ControlPlane {
                    reason: format!("the kernel's answer broke off: {error}"),
                })),
            }
        })
        .boxed();
    Ok(Answer { status, body })
}

/// The error a kernel's non-success answer carries: its problem document, the server's
/// `{"error": "..."}` refusal, or the status alone.
fn answer_error(status: StatusCode, body: &[u8]) -> KernelError {
    let value: Option<Value> = serde_json::from_slice(body).ok();
    let error = value.as_ref().and_then(|value| value.get("error"));
    if let Some(problem) = error.and_then(Value::as_object) {
        let message = problem.get("message").and_then(Value::as_str).unwrap_or("").to_string();
        let code = problem.get("code").and_then(Value::as_str).unwrap_or("");
        let category = problem
            .get("category")
            .and_then(|category| serde_json::from_value(category.clone()).ok())
            .unwrap_or_else(|| category_of(status));
        return KernelError::Core(AdkError::new(
            ErrorComponent::Server,
            category,
            known_code(code),
            message,
        ));
    }
    let message = match error.and_then(Value::as_str) {
        Some(message) => message.to_string(),
        None => String::from_utf8_lossy(body).trim().chars().take(300).collect(),
    };
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => KernelError::Unauthorized {
            reason: format!(
                "{message}; pass the kernel's bearer token (--token or ADK_SERVE_TOKEN), or run \
                 the command as the user that runs the kernel"
            ),
        },
        _ => {
            KernelError::ControlPlane { reason: format!("the kernel answered {status}: {message}") }
        }
    }
}

/// The category a status stands for, when an answer names none.
fn category_of(status: StatusCode) -> ErrorCategory {
    match status.as_u16() {
        400 => ErrorCategory::InvalidInput,
        401 => ErrorCategory::Unauthorized,
        403 => ErrorCategory::Forbidden,
        404 => ErrorCategory::NotFound,
        409 => ErrorCategory::Conflict,
        429 => ErrorCategory::RateLimited,
        501 => ErrorCategory::Unsupported,
        503 => ErrorCategory::Unavailable,
        _ => ErrorCategory::Internal,
    }
}

/// The kernel code `code` names, or the control-plane code for one this client does not know.
fn known_code(code: &str) -> &'static str {
    error::CODES.iter().copied().find(|known| *known == code).unwrap_or(error::CONTROL_PLANE_CODE)
}

/// Percent-encodes `text` for a path segment or a query value.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// The event records of a server-sent event stream: the `data` of each event, keep-alive
/// comments skipped.
fn server_sent_records(
    body: BoxStream<'static, Result<Bytes>>,
) -> BoxStream<'static, Result<EventRecord>> {
    struct Reader {
        body: BoxStream<'static, Result<Bytes>>,
        buffer: Vec<u8>,
        ready: VecDeque<Result<EventRecord>>,
        done: bool,
    }
    let reader = Reader { body, buffer: Vec::new(), ready: VecDeque::new(), done: false };
    stream::unfold(reader, |mut reader| async move {
        loop {
            if let Some(record) = reader.ready.pop_front() {
                return Some((record, reader));
            }
            if reader.done {
                return None;
            }
            match reader.body.next().await {
                Some(Ok(chunk)) => {
                    reader.buffer.extend_from_slice(&chunk);
                    while let Some(end) = event_end(&reader.buffer) {
                        let block: Vec<u8> = reader.buffer.drain(..end.0).collect();
                        reader.buffer.drain(..end.1);
                        if let Some(record) = parse_event(&block) {
                            reader.ready.push_back(record);
                        }
                    }
                }
                Some(Err(error)) => {
                    reader.done = true;
                    reader.ready.push_back(Err(error));
                }
                None => reader.done = true,
            }
        }
    })
    .boxed()
}

/// Where the first event in `buffer` ends: its length and the separator's.
fn event_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n").map(|at| (at, 2));
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n").map(|at| (at, 4));
    match (lf, crlf) {
        (Some(lf), Some(crlf)) => Some(if lf.0 <= crlf.0 { lf } else { crlf }),
        (found, None) | (None, found) => found,
    }
}

/// The record in one event's lines, or `None` for an event with no data.
fn parse_event(block: &[u8]) -> Option<Result<EventRecord>> {
    let text = String::from_utf8_lossy(block);
    let data: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.strip_prefix(' ').unwrap_or(data))
        .collect();
    if data.is_empty() {
        return None;
    }
    Some(serde_json::from_str(&data.join("\n")).map_err(KernelError::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_percent_encoded() {
        assert_eq!(encode("run-1_a.b~c"), "run-1_a.b~c");
        assert_eq!(encode("a/b c?d"), "a%2Fb%20c%3Fd");
    }

    #[tokio::test]
    async fn server_sent_events_split_on_blank_lines_across_chunks() {
        let record = |cursor: u64| {
            serde_json::to_string(&EventRecord {
                cursor,
                at: chrono::Utc::now(),
                event: crate::KernelEvent::Governance { frozen: true, reason: None },
            })
            .unwrap()
        };
        let text = format!(
            ": keep-alive\n\nid: 1\nevent: governance\ndata: {}\n\nid: 2\r\ndata: {}\r\n\r\n",
            record(1),
            record(2)
        );
        let (head, tail) = text.split_at(40);
        let chunks = vec![Ok(Bytes::from(head.to_string())), Ok(Bytes::from(tail.to_string()))];
        let records: Vec<_> = server_sent_records(stream::iter(chunks).boxed())
            .map(|record| record.unwrap().cursor)
            .collect()
            .await;
        assert_eq!(records, vec![1, 2]);
    }

    #[test]
    fn answers_map_to_kernel_errors() {
        let problem = AdkError::from(KernelError::NotFound { pid: "run-9".into() });
        let body = serde_json::to_vec(&problem.to_problem_json()).unwrap();
        let error = AdkError::from(answer_error(StatusCode::NOT_FOUND, &body));
        assert_eq!((error.code, error.category), (error::NOT_FOUND_CODE, ErrorCategory::NotFound));
        assert!(error.message.contains("run-9"));

        let refused =
            answer_error(StatusCode::UNAUTHORIZED, br#"{"error":"missing authorization"}"#);
        assert!(matches!(refused, KernelError::Unauthorized { .. }), "{refused}");

        let other = answer_error(StatusCode::BAD_GATEWAY, b"upstream down");
        assert!(other.to_string().contains("upstream down"), "{other}");
    }

    #[test]
    fn every_kernel_code_reaches_the_client() {
        let refusals = [
            KernelError::AgendaNotFound { item: "agenda-1".into() },
            KernelError::AgendaState {
                item: "agenda-1".into(),
                state: crate::agenda::AgendaStateKind::Rejected,
                action: "approved",
            },
            KernelError::Quota { partition: "work".into(), reason: "max running 2".into() },
        ];
        for refusal in refusals {
            let problem = AdkError::from(refusal);
            let body = serde_json::to_vec(&problem.to_problem_json()).unwrap();
            let status = StatusCode::from_u16(problem.http_status_code()).unwrap();
            let error = AdkError::from(answer_error(status, &body));
            assert_eq!((error.code, error.category), (problem.code, problem.category));
        }
        for code in error::CODES {
            let problem =
                AdkError::new(ErrorComponent::Server, ErrorCategory::Conflict, code, "refused");
            let body = serde_json::to_vec(&problem.to_problem_json()).unwrap();
            assert_eq!(AdkError::from(answer_error(StatusCode::CONFLICT, &body)).code, *code);
        }
        // A code from a crate the kernel composes, which this client does not list.
        let foreign = AdkError::new(
            ErrorComponent::Server,
            ErrorCategory::Conflict,
            "session.not_found",
            "refused",
        );
        let body = serde_json::to_vec(&foreign.to_problem_json()).unwrap();
        let error = AdkError::from(answer_error(StatusCode::CONFLICT, &body));
        assert_eq!((error.code, error.message.as_str()), (error::CONTROL_PLANE_CODE, "refused"));
    }
}
