//! Device pairing (spec 3.3 §10 K2, §11 S-I7): short-lived pairing codes, then scoped,
//! expiring, revocable device tokens, stored hashed.
//!
//! A phone, a tray or a terminal on another machine reaches the kernel's control plane with
//! a device token instead of the operator's own credentials:
//!
//! 1. The person at the kernel issues a code (`adk-rust pair`, `POST /api/kernel/pairing`):
//!    eight characters shown as `ABCD-2345`, valid for ten minutes, naming the scopes the
//!    device gets. The store keeps the code's SHA-256 only.
//! 2. The device claims it (`POST /api/kernel/pairing/claim`, the one route that needs no
//!    caller, since the code is the credential) and receives its token once:
//!    `adkd_<id>_<secret>`. The store keeps the secret's SHA-256 only. A claim takes the code,
//!    so each code pairs one device; twenty wrong codes in a row cancel every waiting code.
//! 3. The device sends the token as `Authorization: Bearer`. Each request is admitted only to
//!    the routes its scopes open ([`device_scope_for`]), as the person who issued the code,
//!    with that person's approval groups, until the token expires or is revoked
//!    (`adk-rust pair revoke`, `POST /api/kernel/devices/{id}/revoke`).
//!
//! | Scope | Routes (under `/api`) |
//! |---|---|
//! | [`DeviceScope::Read`] | `GET /kernel/health`, `GET /kernel/processes`, `GET /kernel/events`, `POST /kernel/attach/{session}`, `GET /kernel/agenda`, `GET /approvals`, `GET /approvals/{id}` |
//! | [`DeviceScope::Decide`] | `POST /approvals/{id}/decide`, low and medium risk; high risk waits for confirming it is the person (spec 3.3 §7.2) |
//! | [`DeviceScope::Stop`] | `POST /kernel/freeze` |
//!
//! Every other route refuses a device token, `unfreeze` included: resuming happens only in
//! the web home or on a paired local terminal (S-I10). Pairing never widens the bind: the
//! loopback default and the rule that a non-loopback bind needs a token or SSO stay as they
//! are (K-I18, S-I7).
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
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use adk_core::{AdkError, Clock, ErrorCategory, ErrorComponent, RequestContext, SystemClock};
use adk_server::auth_bridge::{RequestContextError, RequestContextExtractor};
use async_trait::async_trait;
use axum::http::Method;
use axum::http::request::Parts;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{KernelError, Result, UNAUTHORIZED_CODE};

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

/// Wrong codes in a row after which every waiting code is cancelled.
const MAX_FAILED_CLAIMS: u32 = 20;
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
        }
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
        }
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
}

/// A code just issued (spec 3.3 §10 K2): the only time the code itself is seen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct IssuedCode {
    /// The code, as `ABCD-2345`.
    pub code: String,
    /// When it stops working.
    pub expires_at: DateTime<Utc>,
    /// The scopes the device will get.
    pub scopes: Vec<DeviceScope>,
    /// Where a phone opens the code, when the kernel knows its home's address:
    /// `<home>/pair#code=ABCD-2345`, the code in the fragment so no server log keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
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
    failed_claims: AtomicU32,
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
                failed_claims: AtomicU32::new(0),
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
            failed_claims: AtomicU32::new(shared.failed_claims.load(Ordering::SeqCst)),
        });
        let mut inner = inner;
        change(&mut inner);
        Self { inner: Arc::new(inner) }
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
    /// Returns [`KernelError::Config`] when the owner is empty or the random number
    /// generator fails, or the store's error.
    pub async fn issue(&self, request: IssueCode) -> Result<IssuedCode> {
        if request.owner.trim().is_empty() {
            return Err(KernelError::Config {
                reason: "a pairing code needs the person the device acts for".into(),
            });
        }
        let now = self.now();
        self.inner.store.prune_codes(now).await?;
        let scopes: Vec<DeviceScope> = if request.scopes.is_empty() {
            DeviceScope::ALL.to_vec()
        } else {
            request.scopes.iter().copied().collect::<BTreeSet<_>>().into_iter().collect()
        };
        let code = secret::new_code()?;
        let expires_at = now + chrono::Duration::from_std(self.inner.code_ttl).unwrap_or_default();
        let device_ttl = request.device_ttl.unwrap_or(self.inner.device_ttl).min(MAX_DEVICE_TTL);
        let mut stored =
            PairingCode::new(secret::code_hash(&code), request.owner, device_ttl, now, expires_at);
        stored.groups = request.groups;
        stored.scopes = scopes.clone();
        stored.name = request
            .name
            .map(|name| secret::clean_name(&name, MAX_NAME_CHARS))
            .filter(|name| !name.is_empty());
        self.inner.store.put_code(stored).await?;
        let url = self.inner.home_url.as_ref().map(|home| format!("{home}/pair#code={code}"));
        Ok(IssuedCode { code, expires_at, scopes, url })
    }

    /// Claims `code` for a device called `name` (the name the code was issued with wins) and
    /// returns its token, once.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Unauthorized`] for a wrong, used or expired code; after twenty
    /// wrong codes in a row every waiting code is cancelled. Returns the store's error.
    pub async fn claim(&self, code: &str, name: Option<&str>) -> Result<ClaimedDevice> {
        let now = self.now();
        let taken = self.inner.store.take_code(&secret::code_hash(code)).await?;
        let Some(code) = taken.filter(|code| now < code.expires_at) else {
            let failed = self.inner.failed_claims.fetch_add(1, Ordering::SeqCst) + 1;
            if failed >= MAX_FAILED_CLAIMS {
                self.inner.failed_claims.store(0, Ordering::SeqCst);
                let cleared = self.inner.store.clear_codes().await?;
                tracing::warn!(
                    pairing.cleared = cleared,
                    "too many wrong pairing codes; every waiting code was cancelled"
                );
            }
            return Err(KernelError::Unauthorized {
                reason: "the pairing code is wrong, used or expired: run `adk-rust pair` for a \
                         new one"
                    .into(),
            });
        };
        self.inner.failed_claims.store(0, Ordering::SeqCst);
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
            code.owner,
            token_hash,
            now,
            now + chrono::Duration::from_std(ttl).unwrap_or_default(),
        )
        .with_scopes(code.scopes)
        .with_groups(code.groups);
        self.inner.store.put_device(device.clone()).await?;
        Ok(ClaimedDevice { device: device.view(now), token })
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
        match device_scope_for(method, path) {
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

    /// Removes device `id`: its token stops working at once. Revoking a revoked device
    /// changes nothing.
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
        | ("GET", ["approvals"] | ["approvals", _]) => Some(DeviceScope::Read),
        ("POST", ["approvals", id, "decide"]) if *id != "delegations" => Some(DeviceScope::Decide),
        ("POST", ["kernel", "freeze"]) => Some(DeviceScope::Stop),
        _ => None,
    }
}

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
/// A refused device request answers `401` through `ServerBuilder`, whose extractor errors
/// carry no `403`; [`guard_devices`](crate::control::guard_devices) in front of the app
/// answers `403` first.
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
    use super::*;

    fn pairing() -> Pairing {
        Pairing::new(Arc::new(InMemoryDeviceStore::new()))
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
        let pairing = pairing().with_device_ttl(Duration::from_secs(60));
        let issued = pairing.issue(IssueCode::new("ana", ["operator"])).await.unwrap();
        let claimed = pairing.claim(&issued.code, None).await.unwrap();
        pairing.revoke(&claimed.device.id).await.unwrap();
        let error = pairing.authenticate(&claimed.token).await.unwrap_err();
        assert!(error.to_string().contains("was removed"), "{error}");

        let issued = pairing
            .issue(IssueCode::new("ana", ["operator"]).with_device_ttl(Duration::ZERO))
            .await
            .unwrap();
        let claimed = pairing.claim(&issued.code, None).await.unwrap();
        let error = pairing.authenticate(&claimed.token).await.unwrap_err();
        assert!(error.to_string().contains("pairing ended"), "{error}");
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
