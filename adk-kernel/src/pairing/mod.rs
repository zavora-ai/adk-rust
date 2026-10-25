//! Device pairing (spec 3.3 §10 K2, §11 S-I7): short-lived pairing codes, then scoped,
//! expiring, revocable device tokens, stored hashed.
//!
//! A phone, a tray or a terminal on another machine reaches the kernel's control plane with
//! a device token instead of the operator's own credentials:
//!
//! 1. The person at the kernel issues a code (`adk-rust pair`, `POST /api/kernel/pairing`):
//!    twelve characters shown as `ABCD-EFGH-2345` (60 bits), valid for ten minutes, naming
//!    the scopes the device gets. The store keeps the code's SHA-256 only.
//! 2. The device claims it (`POST /api/kernel/pairing/claim`, the one route that needs no
//!    caller, since the code is the credential) and receives its token once:
//!    `adkd_<id>_<secret>`. The store keeps the secret's SHA-256 only. A claim takes the
//!    code, so each code pairs one device. Every claim is counted in the store before its
//!    code is looked up, so replicas share the count: after twenty claims without a success,
//!    every waiting code is cancelled and claims are refused until the next code is issued.
//! 3. The device sends the token as `Authorization: Bearer` over HTTP. Each request is
//!    admitted only to the routes its scopes open ([`device_scope_for`]), as the person who
//!    issued the code, with that person's approval groups, until the token expires or is
//!    revoked (`adk-rust pair revoke`, `POST /api/kernel/devices/{id}/revoke`). An event
//!    stream a device holds open ends when the device is revoked or its pairing ends: at once
//!    for a revocation on the same kernel, within five seconds for one on another replica.
//!    A decision a device answers is checked for its risk and recorded by the device guard
//!    in front of the routes: the kernel's own HTTP listener runs it, and a server's app
//!    runs it through [`guard_devices`](crate::control::guard_devices); without the guard,
//!    [`DeviceTokenExtractor`] refuses the decision.
//!
//! | Scope | Routes (under `/api`) |
//! |---|---|
//! | [`DeviceScope::Read`] | `GET /kernel/health`, `GET /kernel/processes`, `GET /kernel/events`, `POST /kernel/attach/{session}`, `GET /kernel/agenda`, `GET /approvals`, `GET /approvals/{id}`; `GET /kernel/settings[/{partition}]`, `GET /kernel/away`, `POST /kernel/away/seen`, `GET /kernel/away/lines/{seq}`, `GET /kernel/memory`, `GET /kernel/record`, `GET /kernel/record/check` (spec 3.3 §10 K3–K5) |
//! | [`DeviceScope::Decide`] | `POST /approvals/{id}/decide`, low and medium risk; high risk waits for confirming it is the person (spec 3.3 §7.2) |
//! | [`DeviceScope::Stop`] | `POST /kernel/freeze` |
//!
//! Every other route refuses a device token, `unfreeze` included: resuming happens only in
//! the web home or on a paired local terminal (S-I10). Pairing never widens the bind: the
//! loopback default and the rule that a non-loopback bind needs a token or SSO stay as they
//! are (K-I18, S-I7).
//!
//! A server opens its own routes to devices with [`Pairing::with_device_routes`], a
//! [`DeviceRoutes`] map consulted after [`device_scope_for`], so the kernel never names a
//! surface's paths. The guard checks the risk of, and records, the kernel's own decide route
//! only; a decide route of the server's map is admitted for the server's engine, which
//! enforces what each surface may decide (spec 3.3 §7.2) and records the answer.
//!
//! Every device has a [`DeviceKind`] — phone, terminal or tray — set by whoever issued its
//! code, never by the claim: a request carries it in [`DEVICE_KIND_METADATA`]. A device
//! paired without one is a remote terminal. Surfaces keep what belongs to a device or a person
//! beside it as [`DeviceAttachment`]s (a phone's push subscription, a passkey); revoking a
//! device removes its attachments.
//!
//! | Backend | Type | Feature |
//! |---|---|---|
//! | In-memory | [`InMemoryDeviceStore`] | — |
//! | SQLite | `SqliteDeviceStore` | `sqlite` (default) |
//! | PostgreSQL | `PostgresDeviceStore` | `postgres` |
//! | Redis | `RedisDeviceStore` | `redis` |
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//! use adk_kernel::pairing::{DeviceScope, InMemoryDeviceStore, IssueCode, Pairing};
//!
//! # #[tokio::main(flavor = "current_thread")]
//! # async fn main() -> adk_kernel::Result<()> {
//! let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
//! let issued = pairing
//!     .issue(IssueCode::new("ana", ["operator"]).with_scopes([DeviceScope::Read]).with_name("Ana's phone"))
//!     .await?;
//! let claimed = pairing.claim(&issued.code, None).await?;
//! let device = pairing.authenticate(&claimed.token).await?;
//! assert_eq!(device.owner, "ana");
//! assert!(pairing.claim(&issued.code, None).await.is_err(), "a code pairs one device");
//! # Ok(())
//! # }
//! ```

use std::collections::{BTreeSet, HashMap};
use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{AdkError, Clock, ErrorCategory, ErrorComponent, RequestContext, SystemClock};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use async_trait::async_trait;
use axum::http::Method;
use axum::http::request::Parts;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{KernelError, Result, UNAUTHORIZED_CODE, UNSUPPORTED_CODE};

mod attachments;
mod kinds;
mod memory;
#[cfg(feature = "postgres")]
pub(crate) mod postgres;
#[cfg(feature = "redis")]
pub(crate) mod redis;
pub(crate) mod secret;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub(crate) mod sql;
#[cfg(feature = "sqlite")]
mod sqlite;

pub use attachments::{AttachmentHolder, DeviceAttachment};
pub use kinds::{DeviceKind, DeviceRoutes};
pub use memory::InMemoryDeviceStore;
#[cfg(feature = "postgres")]
pub use postgres::PostgresDeviceStore;
#[cfg(feature = "redis")]
pub use redis::RedisDeviceStore;
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteDeviceStore;

/// The request-context metadata key naming the device a request came from.
pub const DEVICE_ID_METADATA: &str = "adk.device.id";
/// The request-context metadata key with the device's name.
pub const DEVICE_NAME_METADATA: &str = "adk.device.name";
/// The request-context metadata key with the device's scopes, comma-separated.
pub const DEVICE_SCOPES_METADATA: &str = "adk.device.scopes";
/// The request-context metadata key with the device's kind: `phone`, `terminal` or `tray`
/// (spec 3.3 §7.2, §10 K2).
pub const DEVICE_KIND_METADATA: &str = "adk.device.kind";

/// Claims without a success after which the next one cancels every waiting code.
const MAX_FAILED_CLAIMS: u64 = 20;
/// How often an open event stream re-reads its device, for a revocation on another replica.
const DEFAULT_RECHECK: Duration = Duration::from_secs(5);
/// The longest a device name may be.
const MAX_NAME_CHARS: usize = 64;
/// The longest a device's pairing may last.
const MAX_DEVICE_TTL: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// What a device may do (spec 3.3 §10 K2): answer decisions, read, or stop everything.
///
/// # Example
///
/// ```rust
/// use adk_kernel::pairing::DeviceScope;
///
/// assert_eq!(serde_json::to_value(DeviceScope::Decide).unwrap(), "decide");
/// assert_eq!("stop".parse::<DeviceScope>().unwrap(), DeviceScope::Stop);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum DeviceScope {
    /// Answer decisions: allow once, edit or deny (`POST /approvals/{id}/decide`).
    Decide,
    /// Read the process table, the event stream, the agenda, the decisions and the caller's
    /// own sessions.
    Read,
    /// Stop everything (`POST /kernel/freeze`).
    Stop,
}

impl DeviceScope {
    /// Every scope, the default a code grants.
    pub const ALL: [Self; 3] = [Self::Decide, Self::Read, Self::Stop];

    /// The scope's name: `decide`, `read` or `stop`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decide => "decide",
            Self::Read => "read",
            Self::Stop => "stop",
        }
    }
}

impl fmt::Display for DeviceScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for DeviceScope {
    type Err = KernelError;

    fn from_str(text: &str) -> Result<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "decide" => Ok(Self::Decide),
            "read" => Ok(Self::Read),
            "stop" => Ok(Self::Stop),
            other => Err(KernelError::Config {
                reason: format!("unknown device scope `{other}`: use decide, read or stop"),
            }),
        }
    }
}

/// A paired device as the store keeps it (spec 3.3 §10 K2): its token's hash, never the
/// token.
///
/// # Example
///
/// ```rust
/// use adk_kernel::pairing::{DeviceRecord, DeviceScope};
///
/// let now = chrono::Utc::now();
/// let device = DeviceRecord::new("0a1b2c3d4e5f6071", "Ana's phone", "ana", "9f…", now, now + chrono::Duration::days(90))
///     .with_scopes([DeviceScope::Read]);
/// assert!(device.is_active(now));
/// assert!(!device.view(now).scopes.contains(&DeviceScope::Decide));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DeviceRecord {
    /// The device id, sixteen hex digits, part of its token.
    pub id: String,
    /// What the person calls it, such as "Ana's phone".
    pub name: String,
    /// The person the device acts for: the user who issued its code.
    pub owner: String,
    /// The approval groups the device decides with: its owner's when the code was issued.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// What it may do, sorted.
    pub scopes: Vec<DeviceScope>,
    /// The SHA-256 of the token's secret, in hex.
    pub token_hash: String,
    /// When it was paired.
    pub created_at: DateTime<Utc>,
    /// When its token stops working.
    pub expires_at: DateTime<Utc>,
    /// When it was removed, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    /// What it is, as its code's issuer said; none for a remote terminal paired before kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<DeviceKind>,
}

impl DeviceRecord {
    /// A device with every scope and no groups.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        owner: impl Into<String>,
        token_hash: impl Into<String>,
        created_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            owner: owner.into(),
            groups: Vec::new(),
            scopes: DeviceScope::ALL.to_vec(),
            token_hash: token_hash.into(),
            created_at,
            expires_at,
            revoked_at: None,
            kind: None,
        }
    }

    /// Sets what the device is.
    #[must_use]
    pub fn with_kind(mut self, kind: DeviceKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// What the device is: a device paired without a kind is a remote terminal.
    pub fn kind(&self) -> DeviceKind {
        self.kind.unwrap_or(DeviceKind::Terminal)
    }

    /// Sets the scopes, sorted and without repeats.
    #[must_use]
    pub fn with_scopes(mut self, scopes: impl IntoIterator<Item = DeviceScope>) -> Self {
        self.scopes = scopes.into_iter().collect::<BTreeSet<_>>().into_iter().collect();
        self
    }

    /// Sets the approval groups.
    #[must_use]
    pub fn with_groups(mut self, groups: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.groups = groups.into_iter().map(Into::into).collect();
        self
    }

    /// Whether the token works at `now`: not revoked and not expired.
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        self.revoked_at.is_none() && now < self.expires_at
    }

    /// Whether the device holds `scope`.
    pub fn has_scope(&self, scope: DeviceScope) -> bool {
        self.scopes.contains(&scope)
    }

    /// The device as listed: everything but the token's hash.
    pub fn view(&self, now: DateTime<Utc>) -> DeviceView {
        let state = match (self.revoked_at, now < self.expires_at) {
            (Some(_), _) => DeviceState::Revoked,
            (None, true) => DeviceState::Active,
            (None, false) => DeviceState::Expired,
        };
        DeviceView {
            id: self.id.clone(),
            name: self.name.clone(),
            owner: self.owner.clone(),
            scopes: self.scopes.clone(),
            created_at: self.created_at,
            expires_at: self.expires_at,
            revoked_at: self.revoked_at,
            state,
            kind: self.kind(),
        }
    }
}

/// Whether a device's token works (spec 3.3 §10 K2).
///
/// # Example
///
/// ```rust
/// use adk_kernel::pairing::DeviceState;
///
/// assert_eq!(serde_json::to_value(DeviceState::Revoked).unwrap(), "revoked");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum DeviceState {
    /// The token works.
    Active,
    /// The pairing ran out.
    Expired,
    /// The device was removed.
    Revoked,
}

/// A paired device as `GET /kernel/devices` lists it (spec 3.3 §10 K2): no token, no hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DeviceView {
    /// The device id.
    pub id: String,
    /// What the person calls it.
    pub name: String,
    /// The person it acts for.
    pub owner: String,
    /// What it may do.
    pub scopes: Vec<DeviceScope>,
    /// When it was paired.
    pub created_at: DateTime<Utc>,
    /// When its token stops working.
    pub expires_at: DateTime<Utc>,
    /// When it was removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    /// Whether its token works now.
    pub state: DeviceState,
    /// What it is.
    #[serde(default = "terminal")]
    pub kind: DeviceKind,
}

fn terminal() -> DeviceKind {
    DeviceKind::Terminal
}

/// An issued pairing code as the store keeps it (spec 3.3 §10 K2): its hash, never the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PairingCode {
    /// The SHA-256 of the normalized code, in hex.
    pub code_hash: String,
    /// The person the claimed device will act for.
    pub owner: String,
    /// Their approval groups.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    /// The scopes the device gets, sorted.
    pub scopes: Vec<DeviceScope>,
    /// The device's name, when the issuer gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// How long the device's pairing lasts, in seconds.
    pub device_ttl_secs: u64,
    /// When the code was issued.
    pub issued_at: DateTime<Utc>,
    /// When it stops working.
    pub expires_at: DateTime<Utc>,
    /// What the claimed device is, as the issuer said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<DeviceKind>,
}

impl PairingCode {
    /// A code hashed `code_hash` for `owner`, with every scope, no groups and no name.
    pub fn new(
        code_hash: impl Into<String>,
        owner: impl Into<String>,
        device_ttl: Duration,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Self {
        Self {
            code_hash: code_hash.into(),
            owner: owner.into(),
            groups: Vec::new(),
            scopes: DeviceScope::ALL.to_vec(),
            name: None,
            device_ttl_secs: device_ttl.as_secs(),
            issued_at,
            expires_at,
            kind: None,
        }
    }
}

/// Where paired devices and waiting pairing codes are kept (spec 3.3 §10 K2, S-I7), on the
/// backends the kernel's other stores use, with one conformance suite
/// (`device_store_conformance_tests!`, feature `conformance`).
///
/// Neither a code nor a token is ever stored: codes by their hash, devices by their
/// token's hash.
#[async_trait]
pub trait DeviceStore: Send + Sync {
    /// Keeps a waiting code, replacing one with the same hash.
    async fn put_code(&self, code: PairingCode) -> Result<()>;

    /// Removes and returns the code hashed `code_hash`, expired or not; `None` when no code
    /// has it. Two claims of one code take it once.
    async fn take_code(&self, code_hash: &str) -> Result<Option<PairingCode>>;

    /// Removes the codes that expired at or before `before`; returns how many.
    async fn prune_codes(&self, before: DateTime<Utc>) -> Result<usize>;

    /// Removes every waiting code; returns how many.
    async fn clear_codes(&self) -> Result<usize>;

    /// Keeps a device, replacing the one with the same id.
    async fn put_device(&self, device: DeviceRecord) -> Result<()>;

    /// The device `id`, revoked and expired ones included.
    async fn get_device(&self, id: &str) -> Result<Option<DeviceRecord>>;

    /// Every device, oldest pairing first, then by id.
    async fn list_devices(&self) -> Result<Vec<DeviceRecord>>;

    /// Counts one claim attempt and returns how many were counted since the last reset.
    /// Every replica over the store shares the count, and concurrent attempts each get a
    /// distinct count.
    async fn count_claim_attempt(&self) -> Result<u64>;

    /// Starts the count of claim attempts again.
    async fn reset_claim_attempts(&self) -> Result<()>;

    /// Keeps `attachment`, replacing the one with its holder, kind and key (spec 3.3 §5.4).
    ///
    /// The default refuses: a backend that keeps no attachments says so.
    async fn put_attachment(&self, attachment: DeviceAttachment) -> Result<()> {
        let _ = attachment;
        Err(no_attachments())
    }

    /// Keeps `attachment` unless one with its holder, kind and key exists; whether it wrote.
    /// Of concurrent puts of one attachment, from any replica, exactly one writes.
    async fn put_attachment_if_absent(&self, attachment: DeviceAttachment) -> Result<bool> {
        let _ = attachment;
        Err(no_attachments())
    }

    /// `holder`'s attachments of `kind`, by key.
    async fn attachments(
        &self,
        holder: &AttachmentHolder,
        kind: &str,
    ) -> Result<Vec<DeviceAttachment>> {
        let _ = (holder, kind);
        Err(no_attachments())
    }

    /// Removes `holder`'s attachment of `kind` under `key`; whether there was one.
    async fn remove_attachment(
        &self,
        holder: &AttachmentHolder,
        kind: &str,
        key: &str,
    ) -> Result<bool> {
        let _ = (holder, kind, key);
        Err(no_attachments())
    }

    /// Removes every attachment of `holder`; returns how many.
    async fn remove_attachments(&self, holder: &AttachmentHolder) -> Result<usize> {
        let _ = holder;
        Err(no_attachments())
    }
}

/// The error of a [`DeviceStore`] that keeps no attachments.
fn no_attachments() -> KernelError {
    KernelError::Core(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::Unsupported,
        UNSUPPORTED_CODE,
        "this device store keeps no attachments: use the in-memory, SQLite, PostgreSQL or Redis \
         device store",
    ))
}

/// What `adk-rust pair` asks for (spec 3.3 §10 K2): a code for a device acting for `owner`.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_kernel::pairing::{DeviceScope, IssueCode};
///
/// let request = IssueCode::new("ana", ["operator"])
///     .with_scopes([DeviceScope::Read, DeviceScope::Decide])
///     .with_device_ttl(Duration::from_secs(30 * 24 * 3600));
/// assert_eq!(request.scopes.len(), 2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IssueCode {
    /// The person the device will act for.
    pub owner: String,
    /// Their approval groups.
    pub groups: Vec<String>,
    /// The scopes; every scope when empty.
    pub scopes: Vec<DeviceScope>,
    /// The device's name.
    pub name: Option<String>,
    /// How long the pairing lasts; the service's default when `None`.
    pub device_ttl: Option<Duration>,
    /// What the device is; a remote terminal when `None`. Only the issuer sets it.
    pub kind: Option<DeviceKind>,
}

impl IssueCode {
    /// A code for a device acting for `owner` with `groups`, every scope, the default
    /// pairing length and no name.
    pub fn new(
        owner: impl Into<String>,
        groups: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            owner: owner.into(),
            groups: groups.into_iter().map(Into::into).collect(),
            scopes: Vec::new(),
            name: None,
            device_ttl: None,
            kind: None,
        }
    }

    /// Says what the device is (spec 3.3 §7.2): a phone, a remote terminal or a tray.
    #[must_use]
    pub fn with_kind(mut self, kind: DeviceKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Grants `scopes` only.
    #[must_use]
    pub fn with_scopes(mut self, scopes: impl IntoIterator<Item = DeviceScope>) -> Self {
        self.scopes = scopes.into_iter().collect();
        self
    }

    /// Names the device.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets how long the pairing lasts.
    #[must_use]
    pub fn with_device_ttl(mut self, ttl: Duration) -> Self {
        self.device_ttl = Some(ttl);
        self
    }

    /// The request as it is issued and recorded: every scope when none is named, sorted and
    /// without repeats, and the name without control characters, at most 64 characters.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::pairing::{DeviceScope, IssueCode};
    ///
    /// let code = IssueCode::new("ana", ["operator"]).with_name("Ana's\u{7} phone").normalized();
    /// assert_eq!(code.scopes, DeviceScope::ALL);
    /// assert_eq!(code.name.as_deref(), Some("Ana's phone"));
    /// ```
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.scopes = if self.scopes.is_empty() {
            DeviceScope::ALL.to_vec()
        } else {
            self.scopes.iter().copied().collect::<BTreeSet<_>>().into_iter().collect()
        };
        self.name = self
            .name
            .map(|name| secret::clean_name(&name, MAX_NAME_CHARS))
            .filter(|name| !name.is_empty());
        self
    }
}

/// A claimed code whose device is not stored yet: the control plane records the claim
/// first, then stores the device or puts the code back.
pub(crate) struct PendingClaim {
    code: PairingCode,
    device: DeviceRecord,
    token: String,
}

impl PendingClaim {
    /// The device the claim pairs.
    pub(crate) fn device(&self) -> &DeviceRecord {
        &self.device
    }
}

/// The marker the device guard leaves on a request it admitted, so the extractor behind it
/// knows the request's risk was checked and recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceAdmission {
    pub(crate) device: String,
}

/// A code just issued (spec 3.3 §10 K2): the only time the code itself is seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct IssuedCode {
    /// The code, as `ABCD-EFGH-2345`.
    pub code: String,
    /// When it stops working.
    pub expires_at: DateTime<Utc>,
    /// The scopes the device will get.
    pub scopes: Vec<DeviceScope>,
    /// Where a phone opens the code, when the kernel knows its home's address:
    /// `<home>/pair#code=ABCD-EFGH-2345`, the code in the fragment so no server log keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// How long the device that claims the code stays paired, in seconds.
    #[serde(default)]
    pub device_ttl_secs: u64,
}

/// A device just paired (spec 3.3 §10 K2): the only time its token is seen.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ClaimedDevice {
    /// The device.
    pub device: DeviceView,
    /// Its bearer token, `adkd_<id>_<secret>`; keep it secret.
    pub token: String,
}

impl fmt::Debug for ClaimedDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClaimedDevice").field("device", &self.device).finish_non_exhaustive()
    }
}

/// Device pairing over a [`DeviceStore`] (spec 3.3 §10 K2, S-I7): issue and claim codes,
/// authenticate and authorize device tokens, list and revoke devices.
///
/// The control plane serves it when [`ControlPlaneConfig::with_pairing`](crate::control::ControlPlaneConfig::with_pairing)
/// sets it; see the [module documentation](self) for the flow and the scopes.
#[derive(Clone)]
pub struct Pairing {
    inner: Arc<Inner>,
}

struct Inner {
    store: Arc<dyn DeviceStore>,
    #[cfg(feature = "channels")]
    channels: Option<Arc<dyn crate::channels::ChannelBindingStore>>,
    code_ttl: Duration,
    device_ttl: Duration,
    home_url: Option<String>,
    clock: Arc<dyn Clock>,
    recheck: Duration,
    revoked: tokio::sync::broadcast::Sender<String>,
    routes: Option<DeviceRoutes>,
}

impl fmt::Debug for Pairing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pairing")
            .field("code_ttl", &self.inner.code_ttl)
            .field("device_ttl", &self.inner.device_ttl)
            .field("home_url", &self.inner.home_url)
            .finish_non_exhaustive()
    }
}

impl Pairing {
    /// How long a code works by default: ten minutes.
    pub const DEFAULT_CODE_TTL: Duration = Duration::from_secs(10 * 60);
    /// How long a device's pairing lasts by default: ninety days.
    pub const DEFAULT_DEVICE_TTL: Duration = Duration::from_secs(90 * 24 * 60 * 60);

    /// Pairing over `store`, with the default lengths and no home address.
    pub fn new(store: Arc<dyn DeviceStore>) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                #[cfg(feature = "channels")]
                channels: None,
                code_ttl: Self::DEFAULT_CODE_TTL,
                device_ttl: Self::DEFAULT_DEVICE_TTL,
                home_url: None,
                clock: Arc::new(SystemClock),
                recheck: DEFAULT_RECHECK,
                revoked: tokio::sync::broadcast::channel(64).0,
                routes: None,
            }),
        }
    }

    fn rebuild(self, change: impl FnOnce(&mut Inner)) -> Self {
        let inner = Arc::try_unwrap(self.inner).unwrap_or_else(|shared| Inner {
            store: shared.store.clone(),
            #[cfg(feature = "channels")]
            channels: shared.channels.clone(),
            code_ttl: shared.code_ttl,
            device_ttl: shared.device_ttl,
            home_url: shared.home_url.clone(),
            clock: shared.clock.clone(),
            recheck: shared.recheck,
            revoked: shared.revoked.clone(),
            routes: shared.routes.clone(),
        });
        let mut inner = inner;
        change(&mut inner);
        Self { inner: Arc::new(inner) }
    }

    /// Opens the server's own routes to devices (spec 3.3 §10 K2): `routes` names the scope
    /// each needs, after the kernel's [`device_scope_for`]. The device token extractor and
    /// the device guard read it from here.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use axum::http::Method;
    /// use adk_kernel::pairing::{DeviceScope, InMemoryDeviceStore, Pairing};
    ///
    /// let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new())).with_device_routes(Arc::new(
    ///     |method: &Method, path: &str| {
    ///         (method == Method::GET && path.starts_with("/api/surface/")).then_some(DeviceScope::Read)
    ///     },
    /// ));
    /// assert_eq!(pairing.scope_for(&Method::GET, "/api/surface/status"), Some(DeviceScope::Read));
    /// assert_eq!(pairing.scope_for(&Method::POST, "/api/kernel/freeze"), Some(DeviceScope::Stop));
    /// ```
    #[must_use]
    pub fn with_device_routes(self, routes: DeviceRoutes) -> Self {
        self.rebuild(|inner| inner.routes = Some(routes))
    }

    /// The scope a device needs for `method` on `path`: the kernel's routes, then the
    /// server's ([`with_device_routes`](Self::with_device_routes)); `None` when closed.
    pub fn scope_for(&self, method: &Method, path: &str) -> Option<DeviceScope> {
        device_scope_for(method, path)
            .or_else(|| self.inner.routes.as_ref().and_then(|routes| routes(method, path)))
    }

    /// Codes work for `ttl`.
    #[must_use]
    pub fn with_code_ttl(self, ttl: Duration) -> Self {
        self.rebuild(|inner| inner.code_ttl = ttl)
    }

    /// Devices stay paired for `ttl` unless a code says otherwise; at most a year.
    #[must_use]
    pub fn with_device_ttl(self, ttl: Duration) -> Self {
        self.rebuild(|inner| inner.device_ttl = ttl.min(MAX_DEVICE_TTL))
    }

    /// The web home's address, such as `https://kernel.tail1234.ts.net`, so an issued code
    /// carries the URL a phone opens.
    #[must_use]
    pub fn with_home_url(self, url: impl Into<String>) -> Self {
        let url = url.into().trim_end_matches('/').to_string();
        self.rebuild(|inner| inner.home_url = Some(url))
    }

    /// Reads time from `clock`.
    #[must_use]
    pub fn with_clock(self, clock: Arc<dyn Clock>) -> Self {
        self.rebuild(|inner| inner.clock = clock)
    }

    /// How often an open event stream re-reads its device, for a revocation made on another
    /// replica; a revocation made here ends it at once. Defaults to five seconds.
    #[must_use]
    pub fn with_revocation_check(self, interval: Duration) -> Self {
        self.rebuild(|inner| inner.recheck = interval.max(Duration::from_millis(10)))
    }

    /// Keeps chat accounts' pairings in `store` (spec 3.3 §10 K1), so the control plane
    /// accepts, lists and revokes them beside devices.
    #[cfg(feature = "channels")]
    #[must_use]
    pub fn with_channel_bindings(
        self,
        store: Arc<dyn crate::channels::ChannelBindingStore>,
    ) -> Self {
        self.rebuild(|inner| inner.channels = Some(store))
    }

    /// The chat accounts' pairing store, when one is set.
    #[cfg(feature = "channels")]
    pub fn channel_bindings(&self) -> Option<&Arc<dyn crate::channels::ChannelBindingStore>> {
        self.inner.channels.as_ref()
    }

    /// The device store.
    pub fn store(&self) -> &Arc<dyn DeviceStore> {
        &self.inner.store
    }

    /// The web home's address, when set.
    pub fn home_url(&self) -> Option<&str> {
        self.inner.home_url.as_deref()
    }

    /// The time on the service's clock.
    pub fn now(&self) -> DateTime<Utc> {
        self.inner.clock.now()
    }

    /// Issues a code for `request`; the store keeps its hash.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when the owner is empty, the device's pairing would
    /// last no time or more than 365 days, or the random number generator fails, and the
    /// store's error.
    pub async fn issue(&self, request: IssueCode) -> Result<IssuedCode> {
        let device_ttl = self.check(&request)?;
        let request = request.normalized();
        let now = self.now();
        self.inner.store.prune_codes(now).await?;
        let code = secret::new_code()?;
        let expires_at = now + chrono::Duration::from_std(self.inner.code_ttl).unwrap_or_default();
        let mut stored =
            PairingCode::new(secret::code_hash(&code), request.owner, device_ttl, now, expires_at);
        stored.groups = request.groups;
        stored.scopes = request.scopes.clone();
        stored.name = request.name;
        stored.kind = request.kind;
        self.inner.store.put_code(stored).await?;
        // A new code starts the count of claims again, after a lockout too.
        self.inner.store.reset_claim_attempts().await?;
        let url = self.inner.home_url.as_ref().map(|home| format!("{home}/pair#code={code}"));
        Ok(IssuedCode {
            code,
            expires_at,
            scopes: request.scopes,
            url,
            device_ttl_secs: device_ttl.as_secs(),
        })
    }

    /// Checks `request` as [`issue`](Self::issue) does, before anything is recorded of it,
    /// and returns how long its device stays paired.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when the owner is empty or the device's pairing would
    /// last no time or more than 365 days.
    pub(crate) fn check(&self, request: &IssueCode) -> Result<Duration> {
        if request.owner.trim().is_empty() {
            return Err(KernelError::Config {
                reason: "a pairing code needs the person the device acts for".into(),
            });
        }
        let device_ttl = request.device_ttl.unwrap_or(self.inner.device_ttl);
        if device_ttl.is_zero() || device_ttl > MAX_DEVICE_TTL {
            return Err(KernelError::Config {
                reason: format!(
                    "a device stays paired for at most 365 days and longer than none, not {} \
                     days: pass `adk-rust pair --days` between 1 and 365",
                    device_ttl.as_secs() / 86_400
                ),
            });
        }
        Ok(device_ttl)
    }

    /// Claims `code` for a device called `name` (the name the code was issued with wins) and
    /// returns its token, once.
    ///
    /// Every claim is counted in the store before its code is looked up, on every replica
    /// over the store: after twenty claims without a success every waiting code is
    /// cancelled, and claims are refused until the next code is issued. A claim whose device
    /// cannot be stored puts its code back.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Unauthorized`] for a wrong, used or expired code, or once the
    /// claims are locked out. Returns the store's error.
    pub async fn claim(&self, code: &str, name: Option<&str>) -> Result<ClaimedDevice> {
        let pending = self.take_for_claim(code, name).await?;
        self.finish_claim(pending).await
    }

    /// Takes `code` for a claim and makes its device and token, storing neither.
    pub(crate) async fn take_for_claim(
        &self,
        code: &str,
        name: Option<&str>,
    ) -> Result<PendingClaim> {
        let wrong = || KernelError::Unauthorized {
            reason: "the pairing code is wrong, used or expired: run `adk-rust pair` for a new \
                     one"
            .into(),
        };
        let store = &self.inner.store;
        let attempts = store.count_claim_attempt().await?;
        if attempts > MAX_FAILED_CLAIMS {
            // The count stays over the limit until the next code is issued, so every claim
            // in flight and every later one is refused, and a failed clear refuses too.
            let cleared = store.clear_codes().await?;
            if cleared > 0 {
                tracing::warn!(
                    pairing.cleared = cleared,
                    "too many claims without a success; every waiting code was cancelled"
                );
            }
            return Err(KernelError::Unauthorized {
                reason: "too many pairing codes were wrong, so every waiting code was \
                         cancelled: run `adk-rust pair` for a new one"
                    .into(),
            });
        }
        let now = self.now();
        let taken = store.take_code(&secret::code_hash(code)).await?;
        let Some(code) = taken.filter(|code| now < code.expires_at) else {
            return Err(wrong());
        };
        if let Err(error) = store.reset_claim_attempts().await {
            tracing::warn!(error = %error, "the count of pairing claims could not be reset");
        }
        let id = secret::new_device_id()?;
        let (token, token_hash) = secret::new_token(&id)?;
        let name = code
            .name
            .clone()
            .or_else(|| name.map(|name| secret::clean_name(name, MAX_NAME_CHARS)))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Device".to_string());
        let ttl = Duration::from_secs(code.device_ttl_secs).min(MAX_DEVICE_TTL);
        let device = DeviceRecord::new(
            id,
            name,
            code.owner.clone(),
            token_hash,
            now,
            now + chrono::Duration::from_std(ttl).unwrap_or_default(),
        )
        .with_scopes(code.scopes.clone())
        .with_groups(code.groups.clone());
        // The issuer's kind; nothing the claim sends sets it.
        let device = DeviceRecord { kind: code.kind, ..device };
        Ok(PendingClaim { code, device, token })
    }

    /// Stores a taken claim's device and returns its token; puts the code back when the
    /// device cannot be stored.
    pub(crate) async fn finish_claim(&self, pending: PendingClaim) -> Result<ClaimedDevice> {
        if let Err(error) = self.inner.store.put_device(pending.device.clone()).await {
            self.abandon_claim(pending).await;
            return Err(error);
        }
        Ok(ClaimedDevice { device: pending.device.view(self.now()), token: pending.token })
    }

    /// Puts a taken claim's code back, so it can be claimed again.
    pub(crate) async fn abandon_claim(&self, pending: PendingClaim) {
        if let Err(error) = self.inner.store.put_code(pending.code).await {
            tracing::error!(error = %error, "a pairing code could not be put back");
        }
    }

    /// Resolves when device `id` stops working: when it is revoked here, at once; when a
    /// re-read finds it revoked on another replica or gone, each open stream re-reading the
    /// store every five seconds by default; or when its pairing ends. A server ends a
    /// device's open streams with it (spec 3.3 §10 K2, S-I7).
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_kernel::pairing::{InMemoryDeviceStore, IssueCode, Pairing};
    ///
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> adk_kernel::Result<()> {
    /// let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
    /// let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await?;
    /// let device = pairing.claim(&issued.code, None).await?.device;
    /// let ends = pairing.until_device_ends(device.id.clone());
    /// pairing.revoke(&device.id).await?;
    /// ends.await;
    /// # Ok(())
    /// # }
    /// ```
    pub fn until_device_ends(&self, id: String) -> impl Future<Output = ()> + Send + 'static {
        let pairing = self.clone();
        let mut revoked = self.inner.revoked.subscribe();
        async move {
            loop {
                let wait = match pairing.inner.store.get_device(&id).await {
                    Ok(Some(device)) if device.is_active(pairing.now()) => {
                        let left = (device.expires_at - pairing.now()).to_std().unwrap_or_default();
                        left.min(pairing.inner.recheck)
                    }
                    Ok(_) => return,
                    Err(error) => {
                        tracing::warn!(device.id = %id, error = %error, "a device could not be re-read");
                        pairing.inner.recheck
                    }
                };
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    message = revoked.recv() => match message {
                        Ok(gone) if gone == id => return,
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    },
                }
            }
        }
    }

    /// The device `token` belongs to, when the token works.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Unauthorized`] for an unknown, revoked or expired token, and
    /// the store's error.
    pub async fn authenticate(&self, token: &str) -> Result<DeviceRecord> {
        let unknown = || KernelError::Unauthorized {
            reason: "the device token is not one this kernel issued: pair the device again \
                     with `adk-rust pair`"
                .into(),
        };
        let (id, secret_text) = secret::parse_token(token).ok_or_else(unknown)?;
        let device = self.inner.store.get_device(id).await?.ok_or_else(unknown)?;
        if !secret::constant_time_eq(
            secret::digest(secret_text).as_bytes(),
            device.token_hash.as_bytes(),
        ) {
            return Err(unknown());
        }
        if let Some(at) = device.revoked_at {
            return Err(KernelError::Unauthorized {
                reason: format!(
                    "{} was removed on {}: pair it again with `adk-rust pair`",
                    device.name,
                    at.format("%Y-%m-%d")
                ),
            });
        }
        if self.now() >= device.expires_at {
            return Err(KernelError::Unauthorized {
                reason: format!(
                    "{}'s pairing ended on {}: pair it again with `adk-rust pair`",
                    device.name,
                    device.expires_at.format("%Y-%m-%d")
                ),
            });
        }
        Ok(device)
    }

    /// The request context a device token's request runs as, when `method` and `path`
    /// (with or without the `/api` prefix) are a route the device's scopes open: the
    /// device's owner with the owner's approval groups, and the device named in
    /// [`DEVICE_ID_METADATA`] and [`DEVICE_NAME_METADATA`].
    ///
    /// It checks the device's scopes only: a decision's risk, which a device may answer at
    /// low and medium only (spec 3.3 §7.2), is checked by the device guard of the control
    /// plane ([`guard_devices`](crate::control::guard_devices)), which also records it.
    ///
    /// # Errors
    ///
    /// [`authenticate`](Self::authenticate)'s errors, and a `Forbidden` error coded
    /// `kernel.unauthorized` when the route is closed to devices or needs a scope the device
    /// lacks.
    pub async fn authorize(
        &self,
        token: &str,
        method: &Method,
        path: &str,
    ) -> Result<RequestContext> {
        let device = self.authenticate(token).await?;
        match self.scope_for(method, path) {
            Some(scope) if device.has_scope(scope) => Ok(device_context(&device)),
            Some(scope) => Err(forbidden(format!(
                "{} was paired without the `{scope}` scope, which {method} {path} needs; pair it \
                 again with `adk-rust pair --scope {scope}`",
                device.name
            ))),
            None => Err(forbidden(format!(
                "a paired device may not call {method} {path}: devices read, answer decisions \
                 and stop everything; anything else happens in the home or on the kernel's own \
                 terminal"
            ))),
        }
    }

    /// Every paired device, oldest first.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn devices(&self) -> Result<Vec<DeviceRecord>> {
        self.inner.store.list_devices().await
    }

    /// Removes device `id`: its token stops working at once, its attachments are removed, and
    /// an event stream it holds open ends, at once on this kernel and at its next re-read on
    /// another replica (five seconds by default). Revoking a revoked device changes nothing
    /// but removes what is still attached.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Core`] coded `kernel.not_found` for an unknown device, and the
    /// store's error.
    pub async fn revoke(&self, id: &str) -> Result<DeviceRecord> {
        let Some(mut device) = self.inner.store.get_device(id).await? else {
            return Err(KernelError::Core(AdkError::new(
                ErrorComponent::Server,
                ErrorCategory::NotFound,
                crate::error::NOT_FOUND_CODE,
                format!("no paired device {id}: list them with `adk-rust pair list`"),
            )));
        };
        if device.revoked_at.is_none() {
            device.revoked_at = Some(self.now());
            self.inner.store.put_device(device.clone()).await?;
        }
        // Its push subscription and passkey go with it; a store that keeps none has none.
        match self
            .inner
            .store
            .remove_attachments(&AttachmentHolder::Device(device.id.clone()))
            .await
        {
            Ok(_) => {}
            Err(KernelError::Core(error)) if error.category == ErrorCategory::Unsupported => {}
            Err(error) => return Err(error),
        }
        // Event streams the device holds open here end now; elsewhere, at their next re-read.
        let _ = self.inner.revoked.send(device.id.clone());
        Ok(device)
    }
}

/// A `Forbidden` kernel error coded `kernel.unauthorized`, answered `403`.
pub(crate) fn forbidden(message: String) -> KernelError {
    KernelError::Core(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::Forbidden,
        UNAUTHORIZED_CODE,
        message,
    ))
}

/// The request context of `device`'s requests.
fn device_context(device: &DeviceRecord) -> RequestContext {
    let scopes = device.scopes.iter().map(|scope| scope.as_str()).collect::<Vec<_>>().join(",");
    RequestContext {
        user_id: device.owner.clone(),
        scopes: device.groups.clone(),
        metadata: HashMap::from([
            (DEVICE_ID_METADATA.to_string(), device.id.clone()),
            (DEVICE_NAME_METADATA.to_string(), device.name.clone()),
            (DEVICE_SCOPES_METADATA.to_string(), scopes),
            (DEVICE_KIND_METADATA.to_string(), device.kind().as_str().to_string()),
        ]),
    }
}

/// The scope a device needs for `method` on `path`, or `None` when the route is closed to
/// devices (spec 3.3 §7.2, §10 K2, S-I10). `path` may carry the `/api` prefix and a query.
///
/// # Example
///
/// ```rust
/// use axum::http::Method;
/// use adk_kernel::pairing::{DeviceScope, device_scope_for};
///
/// assert_eq!(device_scope_for(&Method::POST, "/api/approvals/a1/decide"), Some(DeviceScope::Decide));
/// assert_eq!(device_scope_for(&Method::GET, "/kernel/processes?limit=5"), Some(DeviceScope::Read));
/// assert_eq!(device_scope_for(&Method::POST, "/api/kernel/freeze"), Some(DeviceScope::Stop));
/// assert_eq!(device_scope_for(&Method::POST, "/api/kernel/unfreeze"), None);
/// assert_eq!(device_scope_for(&Method::POST, "/api/kernel/pairing"), None);
/// ```
pub fn device_scope_for(method: &Method, path: &str) -> Option<DeviceScope> {
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let path = path.strip_prefix(crate::control::paths::API_PREFIX).unwrap_or(path);
    let segments: Vec<&str> = path.split('/').filter(|segment| !segment.is_empty()).collect();
    match (method.as_str(), segments.as_slice()) {
        ("GET", ["kernel", "health" | "processes" | "events" | "agenda"])
        | ("POST", ["kernel", "attach", _])
        // What Today and "Agent Memory and Rules" read, and the device's own place in
        // "While you were away" (spec 3.3 §10 K3–K5); changing a setting or a memory stays
        // closed to devices.
        | (
            "GET",
            ["kernel", "settings" | "away" | "memory" | "record"]
            | ["kernel", "settings", _]
            | ["kernel", "away", "lines", _]
            | ["kernel", "record", "check"],
        )
        | ("POST", ["kernel", "away", "seen"])
        | ("GET", ["approvals"] | ["approvals", _]) => Some(DeviceScope::Read),
        ("POST", ["approvals", id, "decide"]) if *id != "delegations" => Some(DeviceScope::Decide),
        ("POST", ["kernel", "freeze"]) => Some(DeviceScope::Stop),
        _ => None,
    }
}

/// The tool of a take-over, as `adk-harness` asks for one (spec 3.3 §10 H2): a secret the
/// person types in the home or on the phone, never a yes.
pub(crate) const TAKE_OVER_TOOL: &str = "harness:take-over";

/// The device token of a request, when its `Authorization` header carries one.
pub(crate) fn device_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    let value = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let token = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer "))?.trim();
    token.starts_with(secret::TOKEN_PREFIX).then_some(token)
}

/// Admits device tokens as their devices (spec 3.3 §10 K2), each only to the routes its
/// scopes open, and every other request through `fallback`.
///
/// Mount it as the `RequestContextExtractor` of a `ServerBuilder` that serves the kernel's
/// routes, wrapping the extractor it had, so a device token is never mistaken for another
/// credential and never widened: a request that carries one is the device, on loopback too.
/// Layer [`guard_devices`](crate::control::guard_devices) on the built app as well: it
/// checks a device's decision for its risk (low and medium only, spec 3.3 §7.2) and records
/// it, and the extractor refuses every device decision the guard did not admit. A refused
/// device request answers `401` through `ServerBuilder`, whose extractor errors carry no
/// `403`; the guard answers `403` first.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::pairing::{DeviceTokenExtractor, InMemoryDeviceStore, Pairing};
///
/// let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
/// let extractor = DeviceTokenExtractor::new(pairing);
/// # let _ = extractor;
/// ```
#[derive(Clone)]
pub struct DeviceTokenExtractor {
    pairing: Pairing,
    fallback: Option<Arc<dyn RequestContextExtractor>>,
}

impl fmt::Debug for DeviceTokenExtractor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceTokenExtractor")
            .field("pairing", &self.pairing)
            .field("fallback", &self.fallback.is_some())
            .finish()
    }
}

impl DeviceTokenExtractor {
    /// Admits device tokens of `pairing` and nothing else.
    pub fn new(pairing: Pairing) -> Self {
        Self { pairing, fallback: None }
    }

    /// Hands every request without a device token to `fallback`.
    #[must_use]
    pub fn with_fallback(mut self, fallback: Arc<dyn RequestContextExtractor>) -> Self {
        self.fallback = Some(fallback);
        self
    }
}

#[async_trait]
impl RequestContextExtractor for DeviceTokenExtractor {
    async fn extract(
        &self,
        parts: &Parts,
    ) -> std::result::Result<RequestContext, RequestContextError> {
        let Some(token) = device_token(&parts.headers) else {
            return match &self.fallback {
                Some(fallback) => fallback.extract(parts).await,
                None => Err(RequestContextError::MissingAuth),
            };
        };
        let path =
            parts.uri.path_and_query().map_or_else(|| parts.uri.path(), |path| path.as_str());
        match self.pairing.authorize(token, &parts.method, path).await {
            // A device's decision is admitted only behind the guard, which checks its risk
            // and records it (spec 3.3 §7.2, §11).
            Ok(context)
                if self.pairing.scope_for(&parts.method, path) == Some(DeviceScope::Decide)
                    && parts.extensions.get::<DeviceAdmission>().map(|admitted| &admitted.device)
                        != context.metadata.get(DEVICE_ID_METADATA) =>
            {
                Err(RequestContextError::InvalidToken(
                    "a device's decision is admitted only behind adk_kernel::control::guard_devices, \
                     which checks its risk and records it; layer it on the app"
                        .into(),
                ))
            }
            Ok(context) => Ok(context),
            Err(error @ (KernelError::Store { .. } | KernelError::Io(_))) => {
                Err(RequestContextError::ExtractionFailed(error.to_string()))
            }
            Err(error) => Err(RequestContextError::InvalidToken(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    fn pairing() -> Pairing {
        Pairing::new(Arc::new(InMemoryDeviceStore::new()))
    }

    /// An in-memory store that counts code lookups, can make them slow, and can fail a
    /// device write or a clear.
    #[derive(Default)]
    struct Faulty {
        inner: InMemoryDeviceStore,
        lookups: AtomicUsize,
        slow: AtomicBool,
        fail_put_device: AtomicBool,
        fail_clear: AtomicBool,
    }

    fn down() -> KernelError {
        KernelError::Store { backend: "test", reason: "down".into() }
    }

    #[async_trait]
    impl DeviceStore for Faulty {
        async fn put_code(&self, code: PairingCode) -> Result<()> {
            self.inner.put_code(code).await
        }
        async fn take_code(&self, code_hash: &str) -> Result<Option<PairingCode>> {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            if self.slow.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            self.inner.take_code(code_hash).await
        }
        async fn prune_codes(&self, before: DateTime<Utc>) -> Result<usize> {
            self.inner.prune_codes(before).await
        }
        async fn clear_codes(&self) -> Result<usize> {
            if self.fail_clear.load(Ordering::SeqCst) {
                return Err(down());
            }
            self.inner.clear_codes().await
        }
        async fn put_device(&self, device: DeviceRecord) -> Result<()> {
            if self.fail_put_device.swap(false, Ordering::SeqCst) {
                return Err(down());
            }
            self.inner.put_device(device).await
        }
        async fn get_device(&self, id: &str) -> Result<Option<DeviceRecord>> {
            self.inner.get_device(id).await
        }
        async fn list_devices(&self) -> Result<Vec<DeviceRecord>> {
            self.inner.list_devices().await
        }
        async fn count_claim_attempt(&self) -> Result<u64> {
            self.inner.count_claim_attempt().await
        }
        async fn reset_claim_attempts(&self) -> Result<()> {
            self.inner.reset_claim_attempts().await
        }
    }

    /// Concurrent wrong claims are counted before they are checked, so no
    /// more than twenty reach the codes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_wrong_claims_are_counted_before_they_are_checked() {
        let store = Arc::new(Faulty::default());
        store.slow.store(true, Ordering::SeqCst);
        let pairing = Pairing::new(store.clone());
        pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        let mut claims = Vec::new();
        for guess in 0..100 {
            let pairing = pairing.clone();
            claims.push(tokio::spawn(async move {
                pairing.claim(&format!("0000-0000-{guess:04}"), None).await.is_err()
            }));
        }
        for claim in claims {
            assert!(claim.await.unwrap());
        }
        let lookups = store.lookups.load(Ordering::SeqCst);
        assert!(lookups <= MAX_FAILED_CLAIMS as usize, "{lookups} guesses reached the codes");
    }

    /// Replicas over one store share the count of wrong codes.
    #[tokio::test]
    async fn replicas_share_the_count_of_wrong_codes() {
        let store: Arc<dyn DeviceStore> = Arc::new(InMemoryDeviceStore::new());
        let (first, second) = (Pairing::new(store.clone()), Pairing::new(store));
        let issued = first.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        for _ in 0..MAX_FAILED_CLAIMS / 2 {
            assert!(first.claim("0000-0000", None).await.is_err());
            assert!(second.claim("0000-0000", None).await.is_err());
        }
        assert!(second.claim(&issued.code, None).await.is_err(), "the shared lockout fired");
    }

    /// A lockout whose clear fails keeps refusing claims.
    #[tokio::test]
    async fn a_lockout_whose_clear_fails_keeps_refusing() {
        let store = Arc::new(Faulty::default());
        store.fail_clear.store(true, Ordering::SeqCst);
        let pairing = Pairing::new(store.clone());
        let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        for _ in 0..=MAX_FAILED_CLAIMS {
            assert!(pairing.claim("0000-0000", None).await.is_err());
        }
        assert!(pairing.claim(&issued.code, None).await.is_err(), "still locked out");
    }

    /// A claim whose device cannot be stored keeps its code.
    #[tokio::test]
    async fn a_claim_whose_device_cannot_be_stored_keeps_its_code() {
        let store = Arc::new(Faulty::default());
        let pairing = Pairing::new(store.clone());
        let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        store.fail_put_device.store(true, Ordering::SeqCst);
        assert!(matches!(pairing.claim(&issued.code, None).await, Err(KernelError::Store { .. })));
        assert!(pairing.claim(&issued.code, None).await.is_ok(), "the retry pairs");
    }

    /// A lockout holds until the next code is issued, which pairs.
    #[tokio::test]
    async fn a_lockout_holds_until_the_next_code() {
        let pairing = pairing();
        let first = pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        for _ in 0..MAX_FAILED_CLAIMS {
            assert!(pairing.claim("0000-0000-0000", None).await.is_err());
        }
        let error = pairing.claim(&first.code, None).await.unwrap_err();
        assert!(error.to_string().contains("too many"), "{error}");
        let second = pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        assert!(pairing.claim(&first.code, None).await.is_err(), "the first code was cancelled");
        assert!(pairing.claim(&second.code, None).await.is_ok(), "a new code pairs");
    }

    /// The extractor alone never admits a device's decision, which only
    /// the guard in front of the app checks for risk and records.
    #[tokio::test]
    async fn the_extractor_alone_refuses_a_device_decision() {
        let pairing = pairing();
        let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        let token = pairing.claim(&issued.code, None).await.unwrap().token;
        let extractor = DeviceTokenExtractor::new(pairing);
        let request = axum::http::Request::builder()
            .method(Method::POST)
            .uri("/api/approvals/a1/decide")
            .header("authorization", format!("Bearer {token}"))
            .body(())
            .unwrap();
        let (parts, ()) = request.into_parts();
        assert!(extractor.extract(&parts).await.is_err(), "decide needs the guard");
        let request = axum::http::Request::builder()
            .uri("/api/approvals")
            .header("authorization", format!("Bearer {token}"))
            .body(())
            .unwrap();
        let (parts, ()) = request.into_parts();
        assert!(extractor.extract(&parts).await.is_ok(), "read needs no guard");
    }

    #[tokio::test]
    async fn a_code_pairs_one_device_with_its_scopes() {
        let pairing = pairing();
        let issued = pairing
            .issue(IssueCode::new("ana", ["operator"]).with_scopes([DeviceScope::Read]))
            .await
            .unwrap();
        assert_eq!(issued.scopes, [DeviceScope::Read]);
        assert_eq!(issued.url, None);
        let claimed = pairing.claim(&issued.code.to_lowercase(), Some("Pixel")).await.unwrap();
        assert_eq!(claimed.device.name, "Pixel");
        assert_eq!(claimed.device.scopes, [DeviceScope::Read]);
        let device = pairing.authenticate(&claimed.token).await.unwrap();
        assert_eq!(
            (device.owner.as_str(), device.groups.as_slice()),
            ("ana", &["operator".to_string()][..])
        );
        assert!(pairing.claim(&issued.code, None).await.is_err());
    }

    #[tokio::test]
    async fn the_store_never_holds_the_code_or_the_token() {
        let store = Arc::new(InMemoryDeviceStore::new());
        let pairing = Pairing::new(store.clone());
        let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        let claimed = pairing.claim(&issued.code, None).await.unwrap();
        let stored = serde_json::to_string(&store.list_devices().await.unwrap()).unwrap();
        let (_, secret) = secret::parse_token(&claimed.token).unwrap();
        assert!(!stored.contains(secret));
        assert!(!stored.contains(&issued.code));
    }

    #[tokio::test]
    async fn revoked_and_expired_tokens_are_refused() {
        let clock = adk_core::ManualClock::new(Utc::now());
        let pairing =
            pairing().with_device_ttl(Duration::from_secs(60)).with_clock(Arc::new(clock.clone()));
        let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        let claimed = pairing.claim(&issued.code, None).await.unwrap();
        pairing.revoke(&claimed.device.id).await.unwrap();
        let error = pairing.authenticate(&claimed.token).await.unwrap_err();
        assert!(error.to_string().contains("was removed"), "{error}");

        let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        let claimed = pairing.claim(&issued.code, None).await.unwrap();
        clock.advance(Duration::from_secs(61));
        let error = pairing.authenticate(&claimed.token).await.unwrap_err();
        assert!(error.to_string().contains("pairing ended"), "{error}");
        let refused = pairing
            .issue(IssueCode::new("ana", ["operator"]).with_device_ttl(Duration::ZERO))
            .await
            .unwrap_err();
        assert!(matches!(refused, KernelError::Config { .. }), "a zero-length pairing is refused");
    }

    #[tokio::test]
    async fn a_wrong_secret_is_refused() {
        let pairing = pairing();
        let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        let claimed = pairing.claim(&issued.code, None).await.unwrap();
        let last = if claimed.token.ends_with('0') { '1' } else { '0' };
        let forged = format!("{}{last}", &claimed.token[..claimed.token.len() - 1]);
        assert!(matches!(
            pairing.authenticate(&forged).await,
            Err(KernelError::Unauthorized { .. })
        ));
    }

    #[tokio::test]
    async fn twenty_wrong_codes_cancel_every_waiting_code() {
        let pairing = pairing();
        let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        for _ in 0..MAX_FAILED_CLAIMS {
            assert!(pairing.claim("0000-0000", None).await.is_err());
        }
        assert!(pairing.claim(&issued.code, None).await.is_err(), "the lockout cancelled it");
    }

    #[tokio::test]
    async fn authorize_opens_only_the_scoped_routes() {
        let pairing = pairing();
        let issued = pairing
            .issue(IssueCode::new("ana", ["operator"]).with_scopes([DeviceScope::Read]))
            .await
            .unwrap();
        let token = pairing.claim(&issued.code, None).await.unwrap().token;
        let context = pairing.authorize(&token, &Method::GET, "/api/approvals").await.unwrap();
        assert_eq!(context.user_id, "ana");
        assert_eq!(context.scopes, ["operator"]);
        let error =
            pairing.authorize(&token, &Method::POST, "/api/approvals/x/decide").await.unwrap_err();
        assert_eq!(AdkError::from(error).category, ErrorCategory::Forbidden);
        let error =
            pairing.authorize(&token, &Method::POST, "/api/kernel/unfreeze").await.unwrap_err();
        assert_eq!(AdkError::from(error).category, ErrorCategory::Forbidden);
    }
}
