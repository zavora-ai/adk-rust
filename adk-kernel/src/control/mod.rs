//! The control plane (spec 3.2 §7.11).
//!
//! One API, three transports: a Unix domain socket at the kernel home (macOS, Linux) or a
//! named pipe named after it (Windows, §10 W2), each of which trusts the OS user, and HTTP on
//! loopback or, with authentication, on the network. Surfaces hold no state (K-I17); every
//! control action carries an authenticated subject and is audited as `kernel.control`
//! (K-I16).
//!
//! [`router`] is the API. `adk-rust up` merges it into `ServerBuilder` with
//! `add_api_routes`, so the server's auth middleware and `RequestContextExtractor` apply and
//! the routes are served under `/api`; [`ControlPlane`] serves the same router, under the
//! same `/api` prefix, on the socket and on [`ControlPlaneConfig::http`] when set.
//!
//! | Route ([`paths`], under `/api`) | Purpose |
//! |---|---|
//! | `GET /kernel/health` | [`KernelHealth`](crate::KernelHealth) |
//! | `GET /kernel/processes` | the process table, each process with its [`ProcessView`](crate::ProcessView) ([`api::ProcessesQuery`]) |
//! | `POST /kernel/processes` | start a process of a registered target as the caller ([`api::SpawnRequest`]): a turn a surface sends, or a sleeper with a wake set |
//! | `POST /kernel/processes/{pid}/{kill,nice,suspend,resume}` | [`ProcessTable`](crate::ProcessTable) actions |
//! | `GET /kernel/events` | the event bus as server-sent events, from a cursor ([`api::EventsQuery`]) |
//! | `POST /kernel/attach/{session}` | a surface attaches to a session: transcript cursor, pending holds, process |
//! | `POST /kernel/freeze`, `POST /kernel/unfreeze` | [`Kernel::freeze`] and [`Kernel::unfreeze`]; `unfreeze` only on the socket or pipe, a paired local terminal ([`RESUME_NOT_HERE_CODE`] over HTTP, spec 3.3 S-I10) |
//! | `GET /kernel/agenda` | the agenda ([`api::AgendaQuery`]) |
//! | `POST /kernel/agenda/{id}/{approve,reject,schedule}` | [`Agenda`](crate::Agenda) actions as the caller ([`api::RejectRequest`], [`api::ScheduleRequest`]) |
//! | `GET /kernel/drivers`, `POST /kernel/drivers/{id}/{mount,unmount,restart,reapprove}` (feature `drivers`) | the mounted drivers; a mount shown, then approved with `confirm` (`api::MountDriverRequest`, spec 3.2 §7.7) |
//! | `GET /kernel/proc/ledger` (feature `proc`) | the content-free ledger, paged by `seq`, as JSON or as the JSONL `cargo adk eval --ledger` reads |
//! | `GET /kernel/proc/baselines`, `GET /kernel/proc/observations` (feature `proc`) | each partition's last baseline pass, and the observations it offered |
//! | `POST /kernel/pairing`, `POST /kernel/pairing/claim`, `GET /kernel/devices`, `POST /kernel/devices/{id}/revoke` | device pairing (spec 3.3 §10 K2), when [`ControlPlaneConfig::with_pairing`] sets it; see [`pairing`](crate::pairing) |
//! | `POST /kernel/channels/pair`, `GET /kernel/channels`, `POST /kernel/channels/{id}/revoke` (feature `channels`) | chat accounts' pairings (spec 3.3 §10 K1) |
//! | `GET /kernel/partitions` | the partition table ([`api::PartitionsResponse`], spec 3.2 §7.8) |
//! | `POST /kernel/partitions`, `POST /kernel/partitions/{id}/mode` | create or replace a partition; set its mode ([`api::PartitionModeRequest`]) |
//! | `GET /kernel/settings[/{partition}]`, `POST /kernel/settings/{partition}/{quiet-time,spending-limit,mode}` | quiet time, the daily spending limit and the mode while the kernel runs (spec 3.3 §10 K3); see [the routes the surfaces need](self#the-routes-the-surfaces-need) |
//! | `GET /kernel/away`, `POST /kernel/away/seen`, `GET /kernel/away/lines/{seq}` (feature `proc`) | "While you were away" over the `/proc` ledger with a cursor per device (spec 3.3 §10 K4) |
//! | `GET /kernel/memory`, `POST /kernel/memory/facts/{id}/{confirm,fix,forget}`, `POST /kernel/memory/undo` | what the agents remember, with provenance, and its corrections (spec 3.3 §10 K5) |
//! | `GET /kernel/record`, `GET /kernel/record/check` | the latest actions on record, and the check that the record is complete (spec 3.3 §10 K5) |
//! | `GET /kernel/commands`, `POST /kernel/commands/run`, `GET /kernel/rules/always-allow/{id}`, `POST /kernel/rules/always-allow/{id}/{prepare,grant}`, `GET /kernel/sessions/{session}/mode` | the REPL's slash commands, "Always allow" and a session's mode, when [`ControlPlaneConfig::with_commands`] sets a host (spec 3.3 §5.2); see [`CommandHost`] |
//! | `POST /kernel/surface/records` | the records of a surface engine attached to the kernel, on its signed log (spec 3.3 §11); see [`SurfaceRecord`] |
//!
//! The approvals inbox, `/api/runs`, `/api/cron`, `/api/admin/freeze` and `/hooks/{name}`
//! are the existing server routes, unchanged; `POST /api/admin/freeze` delegates to
//! [`Kernel::freeze`] through the kernel's `FreezeDelegate` (§10 S4). The socket serves the
//! approvals inbox beside the kernel routes when the kernel has an approval service, so a
//! held call is decided over the socket as over HTTP; [`decide_guard`] answers each decision
//! under spec 3.3 §7.2 for the surface the transport is.
//!
//! # Authentication and audit
//!
//! Every route needs an authenticated caller: the `Option<RequestContext>` the server's auth
//! middleware puts in the request extensions must be `Some`, or the route answers `401`.
//!
//! | Transport | Caller |
//! |---|---|
//! | Socket | The OS user, with the `operator` scope, when the peer's uid is the socket owner's; any other peer is refused |
//! | Named pipe (Windows) | The OS user, with the `operator` scope, when the token the client opened the pipe with is the kernel's user; the pipe's DACL admits that user alone, and any other client is refused |
//! | HTTP merged into `ServerBuilder` | Whatever the server's `RequestContextExtractor` admits: `LoopbackOperatorExtractor`, `BearerTokenExtractor` or an SSO extractor |
//! | [`ControlPlaneConfig::http`] with an extractor | The extractor's subject |
//! | [`ControlPlaneConfig::http`] on loopback without one | Loopback peers as the OS user with the `operator` scope |
//! | [`ControlPlaneConfig::http`] beyond loopback without one | Refused at start (K-I18) |
//!
//! The routes that change the kernel — `spawn`, `kill`, `nice`, `suspend`, `resume`,
//! `freeze`, `unfreeze`, the agenda's `approve`, `reject` and `schedule`, the driver actions
//! and the partition writes — and the `/proc` routes, which describe the user's activity,
//! need the `operator` scope (`403` otherwise), which the socket, the pipe and a loopback
//! bind grant. `attach` reads only the caller's own sessions. A ledger read is recorded as
//! `kernel.control` with action `proc.ledger`: the ledger leaves the host only through it, and
//! "While you were away" (spec 3.3 §10 K4) serves only the rows of its lines.
//!
//! A paired device's token (spec 3.3 §10 K2, S-I7) admits it over HTTP as the person who
//! paired it, only to the routes its scopes open (`read`, `decide`, `stop`; never
//! `unfreeze`), whatever HTTP admits otherwise: on a loopback HTTP bind a request carrying a
//! device token is the device, not the operator. The socket and the named pipe admit only
//! the kernel's own OS user, as the operator, and read no `Authorization` header, so a
//! device token there changes nothing. Pairing never widens the bind.
//!
//! Every action that changes the kernel or reads a session writes a `kernel.control` record
//! with `subject`, `transport`, `action` and `target` to the kernel's audit trail before it
//! acts (K-I16; `spawn` records the new `pid` once it exists and cancels the process when
//! the record fails), and is refused when the record cannot be written under
//! `AuditFailureMode::Block`, except `freeze`, which takes effect regardless. Reads of the
//! process table, the partition table, the health and the event stream are authenticated
//! and not recorded.
//!
//! # The routes the surfaces need
//!
//! Spec 3.3 §10 K3–K5 adds the routes behind the home's Today and "Agent Memory and Rules"
//! screens. Reading them needs the `operator` scope, which a paired device with `read`
//! has; changing a setting or a memory needs the `operator` scope and no device token: the
//! person at the kernel's socket, pipe or loopback, or signed in to the home. A chat
//! channel reaches no kernel route.
//!
//! | Route | Read | Change | Recorded as `kernel.control` |
//! |---|---|---|---|
//! | Settings (K3) | `operator` or device `read` | the person, no device | `settings.quiet_time`, `settings.spending_limit`, `settings.mode`, with `from` and `to` |
//! | While you were away (K4) | `operator` or device `read` | its own cursor: `operator` or device `read` | no |
//! | Memory (K5) | `operator` or device `read` | the person, no device | `memory.confirm`, `memory.fix`, `memory.forget`, `memory.undo` |
//! | Record (K5) | `operator` or device `read` | — | no |
//!
//! The settings and cursors live in the [`ControlStore`] of
//! [`ControlPlaneConfig::control_store`]; the memory and the record are what a
//! [`MemoryBook`] and a [`RecordBook`] give the kernel, which `adk-rust up` builds over the
//! harness's facts and the kernel's signed audit log.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::AdkError;
use adk_server::auth_bridge::RequestContextExtractor;
use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::error::{KernelError, Result};
use crate::kernel::Kernel;

mod agents;
pub mod api;
#[cfg(feature = "proc")]
mod away;
mod client;
mod commands;
mod decide;
mod memory;
mod pairing;
#[cfg(windows)]
mod pipe;
mod record;
mod routes;
mod server;
pub(crate) mod settings;
#[cfg(unix)]
mod socket;
mod store;
#[cfg(test)]
mod tests;

pub use crate::error::{DECIDE_NOT_HERE_CODE, RESUME_NOT_HERE_CODE};
pub use client::ControlClient;
pub use commands::{
    AlwaysAllowResponse, CommandCall, CommandHost, CommandInfo, CommandReach, CommandsResponse,
    RunCommandRequest, RunCommandResponse, SessionModeResponse, SurfaceRecord,
};
pub use memory::{FactOrigin, MemoryBook, RememberedFact, TidyUp, TidyUpUndo};
#[cfg(feature = "channels")]
pub(crate) use pairing::rated_high;
pub use record::{RecordBook, RecordCheck, RecordLine};
pub use server::ControlPlane;
pub(crate) use store::LastRead;
#[cfg(feature = "postgres")]
pub use store::PostgresControlStore;
#[cfg(feature = "redis")]
pub use store::RedisControlStore;
#[cfg(feature = "sqlite")]
pub use store::SqliteControlStore;
pub use store::{AreaSettings, AreaStart, AwayCursor, Change, ControlStore, InMemoryControlStore};

/// The control plane's route paths, relative to the `/api` prefix they are served under
/// (spec 3.2 §7.11).
pub mod paths {
    /// The prefix every route is served under, on the socket and through
    /// `ServerBuilder::add_api_routes`.
    pub const API_PREFIX: &str = "/api";
    /// `GET`: the kernel's health.
    pub const HEALTH: &str = "/kernel/health";
    /// `GET`: the process table; `POST`: start a process.
    pub const PROCESSES: &str = "/kernel/processes";
    /// `POST`: kill a process.
    pub const KILL: &str = "/kernel/processes/{pid}/kill";
    /// `POST`: move a process to another queue.
    pub const NICE: &str = "/kernel/processes/{pid}/nice";
    /// `POST`: suspend a process.
    pub const SUSPEND: &str = "/kernel/processes/{pid}/suspend";
    /// `POST`: resume a process.
    pub const RESUME: &str = "/kernel/processes/{pid}/resume";
    /// `GET`: the event bus as server-sent events.
    pub const EVENTS: &str = "/kernel/events";
    /// `POST`: attach to a session.
    pub const ATTACH: &str = "/kernel/attach/{session}";
    /// `POST`: freeze execution.
    pub const FREEZE: &str = "/kernel/freeze";
    /// `POST`: lift the freeze.
    pub const UNFREEZE: &str = "/kernel/unfreeze";
    /// `GET`: the agenda (spec 3.2 §7.5).
    pub const AGENDA: &str = "/kernel/agenda";
    /// `POST`: approve an agenda item.
    pub const AGENDA_APPROVE: &str = "/kernel/agenda/{id}/approve";
    /// `POST`: reject an agenda item.
    pub const AGENDA_REJECT: &str = "/kernel/agenda/{id}/reject";
    /// `POST`: schedule an agenda item on a wake set.
    pub const AGENDA_SCHEDULE: &str = "/kernel/agenda/{id}/schedule";
    /// `GET`: the mounted drivers (spec 3.2 §7.7).
    pub const DRIVERS: &str = "/kernel/drivers";
    /// `POST`: show, or with `confirm`, approve and mount a driver's manifest.
    pub const DRIVER_MOUNT: &str = "/kernel/drivers/{id}/mount";
    /// `POST`: unmount a driver.
    pub const DRIVER_UNMOUNT: &str = "/kernel/drivers/{id}/unmount";
    /// `POST`: restart a driver.
    pub const DRIVER_RESTART: &str = "/kernel/drivers/{id}/restart";
    /// `POST`: re-approve a driver's changed declarations.
    pub const DRIVER_REAPPROVE: &str = "/kernel/drivers/{id}/reapprove";
    /// `GET`: the activity ledger (spec 3.2 §7.10).
    pub const PROC_LEDGER: &str = "/kernel/proc/ledger";
    /// `GET`: each partition's baselines (spec 3.2 §7.10).
    pub const PROC_BASELINES: &str = "/kernel/proc/baselines";
    /// `GET`: the observations (spec 3.2 §7.10).
    pub const PROC_OBSERVATIONS: &str = "/kernel/proc/observations";
    /// `POST`: issue a device pairing code (spec 3.3 §10 K2).
    pub const PAIRING: &str = "/kernel/pairing";
    /// `POST`: claim a pairing code; the one route that needs no caller (spec 3.3 §10 K2).
    pub const PAIRING_CLAIM: &str = "/kernel/pairing/claim";
    /// `GET`: the paired devices (spec 3.3 §10 K2).
    pub const DEVICES: &str = "/kernel/devices";
    /// `POST`: remove a paired device (spec 3.3 §10 K2).
    pub const DEVICE_REVOKE: &str = "/kernel/devices/{id}/revoke";
    /// `GET`: the paired chat accounts (spec 3.3 §10 K1).
    pub const CHANNELS: &str = "/kernel/channels";
    /// `POST`: accept the code a chat account was sent (spec 3.3 §10 K1).
    pub const CHANNEL_PAIR: &str = "/kernel/channels/pair";
    /// `POST`: remove a paired chat account (spec 3.3 §10 K1).
    pub const CHANNEL_REVOKE: &str = "/kernel/channels/{id}/revoke";
    /// `POST`: dismiss a chat account waiting to be paired (spec 3.3 §10 K1).
    pub const CHANNEL_DISMISS: &str = "/kernel/channels/waiting/{id}/dismiss";
    /// `GET`: the agents a person added (spec 3.3 §10 K7); no route changes one.
    pub const AGENTS: &str = "/kernel/agents";
    /// `GET`: one agent a person added (spec 3.3 §10 K7).
    pub const AGENT: &str = "/kernel/agents/{id}";
    /// `GET`: the partition table; `POST`: create or replace a partition (spec 3.2 §7.8).
    pub const PARTITIONS: &str = "/kernel/partitions";
    /// `POST`: set a partition's mode (spec 3.2 §7.8, §8.1 `partitions set-mode`).
    pub const PARTITION_MODE: &str = "/kernel/partitions/{id}/mode";
    /// `GET`: every area's settings in force (spec 3.3 §10 K3).
    pub const SETTINGS: &str = "/kernel/settings";
    /// `GET`: one area's settings in force (spec 3.3 §10 K3).
    pub const AREA_SETTINGS: &str = "/kernel/settings/{partition}";
    /// `POST`: set or turn off an area's quiet time (spec 3.3 §10 K3).
    pub const QUIET_TIME: &str = "/kernel/settings/{partition}/quiet-time";
    /// `POST`: set or remove an area's daily spending limit (spec 3.3 §10 K3).
    pub const SPENDING_LIMIT: &str = "/kernel/settings/{partition}/spending-limit";
    /// `POST`: set how much an area's agents act alone (spec 3.3 §10 K3).
    pub const AREA_MODE: &str = "/kernel/settings/{partition}/mode";
    /// `GET`: "While you were away" from the viewer's cursor (spec 3.3 §10 K4).
    pub const AWAY: &str = "/kernel/away";
    /// `POST`: mark "While you were away" seen through a ledger row (spec 3.3 §10 K4).
    pub const AWAY_SEEN: &str = "/kernel/away/seen";
    /// `GET`: one line of "While you were away" with its ledger row (spec 3.3 §10 K4).
    pub const AWAY_LINE: &str = "/kernel/away/lines/{seq}";
    /// `GET`: what the agents remember, with provenance (spec 3.3 §10 K5).
    pub const MEMORY: &str = "/kernel/memory";
    /// `POST`: confirm a remembered fact: "That's right" (spec 3.3 §10 K5).
    pub const FACT_CONFIRM: &str = "/kernel/memory/facts/{id}/confirm";
    /// `POST`: correct a remembered fact: "Fix it" (spec 3.3 §10 K5).
    pub const FACT_FIX: &str = "/kernel/memory/facts/{id}/fix";
    /// `POST`: forget a remembered fact: "Forget it" (spec 3.3 §10 K5).
    pub const FACT_FORGET: &str = "/kernel/memory/facts/{id}/forget";
    /// `POST`: undo an overnight tidy-up of the memory (spec 3.3 §10 K5).
    pub const MEMORY_UNDO: &str = "/kernel/memory/undo";
    /// `GET`: the latest actions on record (spec 3.3 §10 K5).
    pub const RECORD: &str = "/kernel/record";
    /// `GET`: check that every action is on record and nothing was changed or deleted (spec
    /// 3.3 §10 K5).
    pub const RECORD_CHECK: &str = "/kernel/record/check";
    /// `GET`: the REPL's slash commands the kernel offers (spec 3.3 §5.2).
    pub const COMMANDS: &str = "/kernel/commands";
    /// `POST`: run one of them in the caller's session (spec 3.3 §5.2).
    pub const COMMAND_RUN: &str = "/kernel/commands/run";
    /// `GET`: what "Always allow" creates for a waiting decision (spec 3.3 §8.2).
    pub const ALWAYS_ALLOW: &str = "/kernel/rules/always-allow/{id}";
    /// `POST`: make that standing rule, before the decision is answered (spec 3.3 §8.2).
    pub const ALWAYS_ALLOW_PREPARE: &str = "/kernel/rules/always-allow/{id}/prepare";
    /// `POST`: grant it, once the decision was allowed (spec 3.3 §8.2).
    pub const ALWAYS_ALLOW_GRANT: &str = "/kernel/rules/always-allow/{id}/grant";
    /// `GET`: the mode of the caller's session (spec 3.3 §5.2).
    pub const SESSION_MODE: &str = "/kernel/sessions/{session}/mode";
    /// `POST`: write a surface engine's record on the kernel's signed log (spec 3.3 §11).
    pub const SURFACE_RECORDS: &str = "/kernel/surface/records";
}

/// Where the control plane listens and how it authenticates HTTP callers (spec 3.2 §7.11).
///
/// The socket trusts the OS user. HTTP authenticates with the extractor the caller passes:
/// adk-cli passes `LoopbackOperatorExtractor` on loopback and `BearerTokenExtractor`
/// otherwise, with the `operator` scope the approvals inbox routes to; SSO extractors from
/// `adk-auth` serve teams. A non-loopback bind without an extractor is refused (K-I18).
///
/// # Example
///
/// ```rust
/// use std::path::Path;
/// use adk_kernel::control::ControlPlaneConfig;
///
/// let config = ControlPlaneConfig::default();
/// assert_eq!(config.socket_path(Path::new("/home/a/.config/adk-rust")).unwrap(), Path::new("/home/a/.config/adk-rust/kernel.sock"));
/// assert!(config.validate().is_ok());
///
/// let exposed = ControlPlaneConfig::default().with_http("0.0.0.0:8080".parse().unwrap());
/// assert!(exposed.validate().is_err());
/// ```
#[derive(Clone)]
#[non_exhaustive]
pub struct ControlPlaneConfig {
    /// The Unix socket, relative to the kernel home unless absolute; `None` serves no
    /// socket. Defaults to [`DEFAULT_SOCKET`](Self::DEFAULT_SOCKET).
    pub socket: Option<PathBuf>,
    /// An HTTP bind the kernel serves itself; `None` when the routes are merged into a
    /// `ServerBuilder` instead. Defaults to `None`.
    pub http: Option<SocketAddr>,
    /// Authenticates HTTP callers; required on a non-loopback bind (K-I18).
    pub extractor: Option<Arc<dyn RequestContextExtractor>>,
    /// Pairs devices and admits their tokens (spec 3.3 §10 K2); `None` pairs none.
    pub pairing: Option<crate::pairing::Pairing>,
    /// Keeps the settings a person changes while the kernel runs and each device's place in
    /// "While you were away" (spec 3.3 §10 K3, K4). Defaults to an
    /// [`InMemoryControlStore`], so a restart brings back the start settings; `adk-rust up`
    /// keeps them beside its other stores.
    pub control_store: Arc<dyn ControlStore>,
    /// What the agents remember, for the memory routes (spec 3.3 §10 K5); `None` serves
    /// none.
    pub memory: Option<Arc<dyn MemoryBook>>,
    /// The record of actions and its check, for the record routes (spec 3.3 §10 K5); `None`
    /// serves none.
    pub record: Option<Arc<dyn RecordBook>>,
    /// The REPL's slash commands and "Always allow", for the command routes (spec 3.3 §5.2);
    /// `None` serves none.
    pub commands: Option<Arc<dyn CommandHost>>,
    /// The mode each area's agents started in, which the settings routes report until the
    /// person changes it (spec 3.3 §10 K3). A partition's record gives its own.
    pub start_modes: BTreeMap<crate::partition::PartitionId, crate::partition::PartitionMode>,
    /// The modes the settings routes refuse to set, each with why, such as Auto on a kernel
    /// served beyond loopback without a sandbox (spec 3.3 §10 K3); a change to one made
    /// before is not applied.
    pub refused_modes: Vec<(crate::partition::PartitionMode, String)>,
    /// The in-memory store `Default` made, to tell it from one the caller set.
    default_control_store: Arc<dyn ControlStore>,
}

impl ControlPlaneConfig {
    /// The socket's file name under the kernel home.
    pub const DEFAULT_SOCKET: &'static str = "kernel.sock";

    /// Serves the socket at `path`, relative to the kernel home unless absolute.
    #[must_use]
    pub fn with_socket(mut self, path: impl Into<PathBuf>) -> Self {
        self.socket = Some(path.into());
        self
    }

    /// Serves no socket.
    #[must_use]
    pub fn without_socket(mut self) -> Self {
        self.socket = None;
        self
    }

    /// Serves HTTP on `addr`.
    #[must_use]
    pub fn with_http(mut self, addr: SocketAddr) -> Self {
        self.http = Some(addr);
        self
    }

    /// Authenticates HTTP callers with `extractor`.
    #[must_use]
    pub fn with_extractor(mut self, extractor: Arc<dyn RequestContextExtractor>) -> Self {
        self.extractor = Some(extractor);
        self
    }

    /// Pairs devices with `pairing` and admits their tokens, each to the routes its scopes
    /// open (spec 3.3 §10 K2). It changes no bind: a non-loopback bind still needs an
    /// extractor (S-I7, K-I18).
    #[must_use]
    pub fn with_pairing(mut self, pairing: crate::pairing::Pairing) -> Self {
        self.pairing = Some(pairing);
        self
    }

    /// Keeps changed settings and away cursors in `store` (spec 3.3 §10 K3, K4).
    #[must_use]
    pub fn with_control_store(mut self, store: Arc<dyn ControlStore>) -> Self {
        self.control_store = store;
        self
    }

    /// Serves the memory routes over `book` (spec 3.3 §10 K5).
    #[must_use]
    pub fn with_memory(mut self, book: Arc<dyn MemoryBook>) -> Self {
        self.memory = Some(book);
        self
    }

    /// Serves the record routes over `book` (spec 3.3 §10 K5).
    #[must_use]
    pub fn with_record(mut self, book: Arc<dyn RecordBook>) -> Self {
        self.record = Some(book);
        self
    }

    /// Serves the REPL's slash commands and "Always allow" over `host` (spec 3.3 §5.2), so a
    /// terminal attached to the kernel has the commands the REPL has.
    #[must_use]
    pub fn with_commands(mut self, host: Arc<dyn CommandHost>) -> Self {
        self.commands = Some(host);
        self
    }

    /// Reports `mode` as the mode `partition`'s agents started in (spec 3.3 §10 K3).
    #[must_use]
    pub fn with_start_mode(
        mut self,
        partition: impl Into<crate::partition::PartitionId>,
        mode: crate::partition::PartitionMode,
    ) -> Self {
        self.start_modes.insert(partition.into(), mode);
        self
    }

    /// Refuses to set `mode` through the settings routes, saying `reason` (spec 3.3 §10 K3):
    /// the start checks of `adk-rust up` hold for a mode changed while it runs.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::PartitionMode;
    /// use adk_kernel::control::ControlPlaneConfig;
    ///
    /// let config = ControlPlaneConfig::default()
    ///     .with_refused_mode(PartitionMode::Auto, "Go ahead needs --sandbox beyond loopback");
    /// assert_eq!(config.refused_modes.len(), 1);
    /// ```
    #[must_use]
    pub fn with_refused_mode(
        mut self,
        mode: crate::partition::PartitionMode,
        reason: impl Into<String>,
    ) -> Self {
        self.refused_modes.push((mode, reason.into()));
        self
    }

    /// Whether the control store is still the in-memory one `Default` made, so a restart
    /// brings back the start settings.
    pub fn has_default_control_store(&self) -> bool {
        std::ptr::addr_eq(
            Arc::as_ptr(&self.control_store),
            Arc::as_ptr(&self.default_control_store),
        )
    }

    /// Why the settings routes refuse to set `mode`, or `None`.
    pub fn refused_mode(&self, mode: crate::partition::PartitionMode) -> Option<&str> {
        self.refused_modes
            .iter()
            .find(|(refused, _)| *refused == mode)
            .map(|(_, reason)| reason.as_str())
    }

    /// The socket's path for the kernel at `home`, or `None` when no socket is served.
    ///
    /// It is the socket under `home` when that path fits a Unix socket address (103 bytes on
    /// macOS, 107 on Linux). When it does not, the kernel and its clients use
    /// `adk-rust-<tag>/<socket name>` under the user's own temp directory instead, the tag
    /// derived from the resolved home, so each derives the same path from the same home
    /// (spec 3.3 G18). Windows serves a named pipe and keeps the path under the home.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::path::Path;
    /// use adk_kernel::control::ControlPlaneConfig;
    ///
    /// let config = ControlPlaneConfig::default();
    /// let short = config.socket_path(Path::new("/home/a/.config/adk-rust")).unwrap();
    /// assert_eq!(short, Path::new("/home/a/.config/adk-rust/kernel.sock"));
    /// # #[cfg(unix)]
    /// # {
    /// let long = Path::new("/srv").join("a-kernel-home-with-a-long-name/".repeat(4));
    /// let socket = config.socket_path(&long).unwrap();
    /// assert!(!socket.starts_with(&long) && socket.ends_with("kernel.sock"));
    /// # }
    /// ```
    pub fn socket_path(&self, home: &Path) -> Option<PathBuf> {
        let path = home.join(self.socket.as_ref()?);
        #[cfg(unix)]
        let path = socket::place(path);
        Some(path)
    }

    /// The named pipe Windows serves in place of the socket (spec 3.2 §10 W2), or `None`
    /// when no socket is served: `\\.\pipe\adk-rust-kernel-<tag>`, where the tag is
    /// derived from the absolute socket path, so each kernel home has its own pipe. Pipe
    /// names are not files, so the name is computed the same way on every platform.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::path::Path;
    /// use adk_kernel::control::ControlPlaneConfig;
    ///
    /// let config = ControlPlaneConfig::default();
    /// let pipe = config.pipe_name(Path::new("/home/a/.config/adk-rust")).unwrap();
    /// assert!(pipe.starts_with(r"\\.\pipe\adk-rust-kernel-"));
    /// assert_ne!(Some(pipe), config.pipe_name(Path::new("/srv/kernel")));
    /// assert_eq!(config.without_socket().pipe_name(Path::new("/srv/kernel")), None);
    /// ```
    pub fn pipe_name(&self, home: &Path) -> Option<String> {
        let socket = home.join(self.socket.as_ref()?);
        Some(format!(r"\\.\pipe\adk-rust-kernel-{}", crate::path_tag(&socket)))
    }

    /// Refuses a non-loopback HTTP bind without an extractor (K-I18).
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] naming the bind.
    pub fn validate(&self) -> Result<()> {
        match self.http {
            Some(addr) if !addr.ip().is_loopback() && self.extractor.is_none() => {
                Err(KernelError::Config {
                    reason: format!(
                        "serving the control plane on {addr} needs authentication: pass a \
                         bearer token or SSO extractor, or bind 127.0.0.1"
                    ),
                })
            }
            _ => Ok(()),
        }
    }
}

impl Default for ControlPlaneConfig {
    fn default() -> Self {
        let control_store: Arc<dyn ControlStore> = Arc::new(InMemoryControlStore::new());
        Self {
            socket: Some(PathBuf::from(Self::DEFAULT_SOCKET)),
            http: None,
            extractor: None,
            pairing: None,
            default_control_store: Arc::clone(&control_store),
            control_store,
            memory: None,
            record: None,
            commands: None,
            start_modes: BTreeMap::new(),
            refused_modes: Vec::new(),
        }
    }
}

impl fmt::Debug for ControlPlaneConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControlPlaneConfig")
            .field("socket", &self.socket)
            .field("http", &self.http)
            .field("extractor", &self.extractor.is_some())
            .field("pairing", &self.pairing)
            .field("memory", &self.memory.is_some())
            .field("record", &self.record.is_some())
            .field("commands", &self.commands.is_some())
            .field("start_modes", &self.start_modes)
            .field("refused_modes", &self.refused_modes)
            .finish()
    }
}

/// The transport a control action arrived on, as the `kernel.control` audit record names it
/// (spec 3.2 §9.3).
///
/// # Example
///
/// ```rust
/// use adk_kernel::control::ControlTransport;
///
/// assert_eq!(serde_json::to_value(ControlTransport::Socket).unwrap(), "socket");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum ControlTransport {
    /// The Unix socket.
    Socket,
    /// HTTP.
    Http,
    /// The named pipe on Windows (spec 3.2 §10 W2).
    Pipe,
}

/// The control plane's routes over `kernel` (spec 3.2 §7.11), relative to the `/api`
/// prefix: merge them with `ServerBuilder::add_api_routes`, whose auth middleware puts the
/// caller's `RequestContext` in the request extensions.
///
/// # Example
///
/// ```rust,no_run
/// # fn example(kernel: adk_kernel::Kernel, builder: adk_server::ServerBuilder) {
/// let builder = builder.add_api_routes(adk_kernel::control::router(kernel));
/// # let _ = builder;
/// # }
/// ```
pub fn router(kernel: Kernel) -> Router {
    routes::routes(Arc::new(kernel))
}

/// The claim route of `kernel`'s device pairing (spec 3.3 §10 K2) at
/// `/api/kernel/pairing/claim`, which needs no caller since the code is the credential:
/// merge it with `ServerBuilder::add_root_routes`, outside the auth middleware and inside
/// the host and origin checks. Empty when the kernel pairs no devices.
///
/// A server that serves the kernel's routes to devices needs all three pieces: the routes
/// behind a [`DeviceTokenExtractor`](crate::pairing::DeviceTokenExtractor), this claim
/// route, and [`guard_devices`] around the built app, which checks a device's decision for
/// its risk and records it; the extractor refuses a device's decision the guard did not
/// admit.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::pairing::DeviceTokenExtractor;
///
/// # fn example(kernel: adk_kernel::Kernel, config: adk_server::ServerConfig, bearer: Arc<dyn adk_server::auth_bridge::RequestContextExtractor>) {
/// let pairing = kernel.config().control.pairing.clone().expect("the kernel pairs devices");
/// let extractor = DeviceTokenExtractor::new(pairing).with_fallback(bearer);
/// let app = adk_server::ServerBuilder::new(config.with_request_context(Arc::new(extractor)))
///     .add_api_routes(adk_kernel::control::router(kernel.clone()))
///     .add_root_routes(adk_kernel::control::claim_router(kernel.clone()))
///     .build();
/// let app = adk_kernel::control::guard_devices(&kernel, app);
/// # let _ = app;
/// # }
/// ```
pub fn claim_router(kernel: Kernel) -> Router {
    let backend: Arc<dyn routes::ControlBackend> = Arc::new(kernel);
    match backend.pairing() {
        Some(_) => pairing::claim_routes(backend),
        None => Router::new(),
    }
}

/// `app` with every request that carries a device token checked against `kernel`'s pairing
/// first (spec 3.3 §10 K2, S-I7): `401` for an unknown, expired or revoked token, `403` for
/// a route its scopes do not open, a decision it answers on `POST /approvals/{id}/decide`
/// limited to low and medium risk and recorded. The server's own routes are those of
/// [`Pairing::with_device_routes`](crate::pairing::Pairing::with_device_routes); a decision
/// on one of them is admitted for the server's engine, which applies what each surface may
/// decide. Layer it on the whole app that serves the kernel's routes, so no route outside
/// them admits a device either; requests without a device token pass untouched.
///
/// # Example
///
/// ```rust,no_run
/// # fn example(kernel: adk_kernel::Kernel, app: axum::Router) {
/// let app = adk_kernel::control::guard_devices(&kernel, app);
/// # let _ = app;
/// # }
/// ```
pub fn guard_devices(kernel: &Kernel, app: Router) -> Router {
    pairing::guard(Arc::new(kernel.clone()), app)
}

/// `kernel`'s check of every decision on `POST /api/approvals/{id}/decide` (spec 3.3 §7.2,
/// §11 S-I11, S-I13): the answer is checked under §7.2 for the surface the server
/// established — the kernel's socket or pipe as a paired local terminal, a paired device by
/// its kind, an operator credential over HTTP as `api` — and refused with
/// [`DECIDE_NOT_HERE_CODE`] where that surface may not give it; never by a surface a client
/// declares. An admitted decision is recorded before it reaches the inbox, and a suggested
/// agent it answers is added or ended at once as that surface. The kernel's own control plane
/// installs it; a server that serves the kernel's inbox installs it with
/// `ApprovalEndpoints::with_decide_guard`.
///
/// # Example
///
/// ```rust,no_run
/// use adk_server::approvals::ApprovalEndpoints;
///
/// # fn example(kernel: adk_kernel::Kernel, service: std::sync::Arc<dyn adk_core::ApprovalService>, builder: adk_server::ServerBuilder) {
/// let endpoints =
///     ApprovalEndpoints::new(service).with_decide_guard(adk_kernel::control::decide_guard(&kernel));
/// let builder = builder.with_approval_endpoints(endpoints);
/// # let _ = builder;
/// # }
/// ```
pub fn decide_guard(kernel: &Kernel) -> Arc<dyn adk_server::approvals::DecideGuard> {
    Arc::new(decide::KernelDecideGuard::new(Arc::new(kernel.clone())))
}

impl IntoResponse for KernelError {
    fn into_response(self) -> Response {
        let error = AdkError::from(self);
        let status = StatusCode::from_u16(error.http_status_code())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(error.to_problem_json())).into_response()
    }
}
