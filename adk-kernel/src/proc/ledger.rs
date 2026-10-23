//! The content-free activity ledger: what writers submit, what is stored, the subject key
//! and the writer (spec 3.2 §7.10, §9.2 K-I19).

use std::fmt;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use adk_core::{AdkError, Clock, ErrorCategory, ErrorComponent, SystemClock};
use async_trait::async_trait;
use chrono::{DateTime, FixedOffset, NaiveDate, Offset as _, TimeZone as _, Utc};
use hmac::Mac as _;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::LedgerStore;
use crate::error::{KernelError, Result};
use crate::partition::PartitionId;

/// Error code of a ledger row the ledger refuses (spec 3.2 §9.2 K-I19).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::proc::LEDGER_INVALID_CODE, "kernel.ledger.invalid_row");
/// ```
pub const LEDGER_INVALID_CODE: &str = "kernel.ledger.invalid_row";

/// The agent label of the rows the kernel writes about its own work (spec 3.2 §7.10).
///
/// The baselines leave these rows out, so the kernel's maintenance does not move the
/// user's activity.
pub const KERNEL_AGENT: &str = "kernel";

/// Most entries a row's `meta` holds (spec 3.2 §7.10: small, schema-checked).
pub const MAX_META_ENTRIES: usize = 8;

/// What a ledger row records (spec 3.2 §7.10).
///
/// Serialized as a snake_case string; a name this enum does not know round-trips through
/// [`Other`](Self::Other).
///
/// | Kind | Written by | When |
/// |---|---|---|
/// | `intent` | the harness | a run starts on a person's message |
/// | `tool_call` | the harness | a tool call returned, with its duration |
/// | `approval`, `rejection` | the harness's audit records | an approver granted or refused a held call |
/// | `denial`, `held` | the harness's audit records | the policy denied a call, or held it for approval |
/// | `turn` | the harness's `harness.spend` record | a harness run ended, with its counts and cost |
/// | `loop_guard` | the harness's `harness.loop.detected` record | the loop guard ended a turn |
/// | `audit` | the harness's other audit records | a mode change, a rule, a goal step |
/// | `process` | the kernel, from `KernelEvent::Process` | a process changed state |
/// | `wake`, `spend`, `rule_hit`, `governance` | the kernel, from its events | a wake was routed, spend committed, a rule hit, the kill switch changed |
/// | `held`, `decided` | the kernel, from `KernelEvent::Held` and `Decided` | a process held a call, a held call was decided |
/// | `observation` | the kernel | a baseline pass recorded an observation |
/// | `proposal` | the kernel, from `KernelEvent::Agenda` | an agenda item changed state (spec 3.2 §7.5) |
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::LedgerKind;
///
/// assert_eq!(LedgerKind::ToolCall.as_str(), "tool_call");
/// assert_eq!(LedgerKind::from("tool_call".to_string()), LedgerKind::ToolCall);
/// assert_eq!(LedgerKind::from("review".to_string()), LedgerKind::Other("review".into()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
#[non_exhaustive]
pub enum LedgerKind {
    /// A run started on a person's message.
    Intent,
    /// A tool call returned.
    ToolCall,
    /// An approver granted a held call.
    Approval,
    /// An approver refused a held call.
    Rejection,
    /// The policy denied a call.
    Denial,
    /// A call was held for approval.
    Held,
    /// A held call was decided.
    Decided,
    /// A standing rule allowed a call.
    RuleHit,
    /// A baseline pass recorded an observation.
    Observation,
    /// A wake event was routed.
    Wake,
    /// An agent proposed a follow-up.
    Proposal,
    /// A process changed state.
    Process,
    /// Spend was committed to a process.
    Spend,
    /// A harness run ended.
    Turn,
    /// The loop guard ended a turn.
    LoopGuard,
    /// The kill switch changed.
    Governance,
    /// Another audit record.
    Audit,
    /// A kind this enum does not name.
    Other(String),
}

impl LedgerKind {
    /// The wire name.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Intent => "intent",
            Self::ToolCall => "tool_call",
            Self::Approval => "approval",
            Self::Rejection => "rejection",
            Self::Denial => "denial",
            Self::Held => "held",
            Self::Decided => "decided",
            Self::RuleHit => "rule_hit",
            Self::Observation => "observation",
            Self::Wake => "wake",
            Self::Proposal => "proposal",
            Self::Process => "process",
            Self::Spend => "spend",
            Self::Turn => "turn",
            Self::LoopGuard => "loop_guard",
            Self::Governance => "governance",
            Self::Audit => "audit",
            Self::Other(name) => name,
        }
    }
}

impl From<String> for LedgerKind {
    fn from(value: String) -> Self {
        match value.as_str() {
            "intent" => Self::Intent,
            "tool_call" => Self::ToolCall,
            "approval" => Self::Approval,
            "rejection" => Self::Rejection,
            "denial" => Self::Denial,
            "held" => Self::Held,
            "decided" => Self::Decided,
            "rule_hit" => Self::RuleHit,
            "observation" => Self::Observation,
            "wake" => Self::Wake,
            "proposal" => Self::Proposal,
            "process" => Self::Process,
            "spend" => Self::Spend,
            "turn" => Self::Turn,
            "loop_guard" => Self::LoopGuard,
            "governance" => Self::Governance,
            "audit" => Self::Audit,
            _ => Self::Other(value),
        }
    }
}

impl From<LedgerKind> for String {
    fn from(value: LedgerKind) -> Self {
        match value {
            LedgerKind::Other(name) => name,
            known => known.as_str().to_string(),
        }
    }
}

impl fmt::Display for LedgerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a writer tells the ledger happened (spec 3.2 §7.10).
///
/// An activity may name its `subject` — what it was about, such as a tool call's
/// fingerprint or a process id — in the clear: the [`Ledger`] stores only its HMAC under
/// the kernel's key, never the subject itself. Everything else is stored as given, so it
/// must be a label, a count or a duration; [`Ledger::entry`] refuses anything else.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_kernel::proc::{Activity, LedgerKind};
///
/// let activity = Activity::new(LedgerKind::ToolCall, "urn:adk:agent:adk-rust/harness")
///     .with_effect("read_only")
///     .with_duration(Duration::from_millis(182))
///     .with_subject("read_file\u{1f}{\"path\":\"src/main.rs\"}")
///     .with_count("attempt", 1);
/// assert_eq!(activity.kind, LedgerKind::ToolCall);
/// assert_eq!(activity.meta["attempt"], 1);
/// ```
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Activity {
    /// What happened.
    pub kind: LedgerKind,
    /// The agent that acted: its id, or [`KERNEL_AGENT`] for the kernel's own work.
    pub agent: String,
    /// The partition the activity belongs to.
    pub partition: PartitionId,
    /// The effect class of the action, such as `read_only`.
    pub effect: Option<String>,
    /// How long it took.
    pub duration: Option<Duration>,
    /// What it was about, in the clear; stored only as its HMAC.
    pub subject: Option<String>,
    /// Counts and flags; numbers feed the baselines.
    pub meta: Map<String, Value>,
    /// The trace or invocation the activity belongs to.
    pub trace_id: Option<String>,
    /// When it happened; the ledger's clock when unset.
    pub at: Option<DateTime<Utc>>,
}

impl Activity {
    /// An activity of `kind` by `agent`, in the default partition.
    pub fn new(kind: LedgerKind, agent: impl Into<String>) -> Self {
        Self {
            kind,
            agent: agent.into(),
            partition: PartitionId::default(),
            effect: None,
            duration: None,
            subject: None,
            meta: Map::new(),
            trace_id: None,
            at: None,
        }
    }

    /// Sets the partition.
    #[must_use]
    pub fn with_partition(mut self, partition: impl Into<PartitionId>) -> Self {
        self.partition = partition.into();
        self
    }

    /// Sets the effect class.
    #[must_use]
    pub fn with_effect(mut self, effect: impl Into<String>) -> Self {
        self.effect = Some(effect.into());
        self
    }

    /// Sets how long it took.
    #[must_use]
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    /// Sets what it was about; the ledger stores only its HMAC.
    #[must_use]
    pub fn with_subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }

    /// Adds a count to `meta`.
    #[must_use]
    pub fn with_count(mut self, key: impl Into<String>, value: u64) -> Self {
        self.meta.insert(key.into(), Value::from(value));
        self
    }

    /// Adds a number to `meta`; a value that is not finite is left out.
    #[must_use]
    pub fn with_number(mut self, key: impl Into<String>, value: f64) -> Self {
        if let Some(number) = serde_json::Number::from_f64(value) {
            self.meta.insert(key.into(), Value::Number(number));
        }
        self
    }

    /// Adds a flag to `meta`.
    #[must_use]
    pub fn with_flag(mut self, key: impl Into<String>, value: bool) -> Self {
        self.meta.insert(key.into(), Value::Bool(value));
        self
    }

    /// Sets the trace or invocation it belongs to.
    #[must_use]
    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    /// Sets when it happened.
    #[must_use]
    pub fn at(mut self, at: DateTime<Utc>) -> Self {
        self.at = Some(at);
        self
    }
}

/// One row of the ledger as stored and exported (spec 3.2 §7.10): `ts, partition, agent,
/// kind, effect, duration_ms, subject_hash, meta, trace_id`, plus the store's `seq`.
///
/// It serializes as one camelCase JSON object, the line format `cargo adk eval --ledger`
/// reads (`adk_eval::ledger::LedgerRow`, which ignores `seq`). `ts` carries the offset of
/// the user's local time when it was written, so a day and a time of day read in it are the
/// user's.
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::LedgerEntry;
///
/// let line = r#"{"seq":7,"ts":"2026-10-22T19:20:04+03:00","partition":"default","agent":"kernel","kind":"wake","subjectHash":"hmac-sha256:00"}"#;
/// let entry: LedgerEntry = serde_json::from_str(line).unwrap();
/// assert_eq!(entry.kind, "wake");
/// assert_eq!(entry.ts.offset().local_minus_utc(), 3 * 3600);
/// assert_eq!(serde_json::to_string(&entry).unwrap(), line);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct LedgerEntry {
    /// The store's sequence number: increasing in append order, the paging cursor.
    #[serde(default)]
    pub seq: u64,
    /// When it happened, in the user's local offset, to the millisecond.
    pub ts: DateTime<FixedOffset>,
    /// The partition.
    pub partition: PartitionId,
    /// The agent that acted.
    pub agent: String,
    /// What happened ([`LedgerKind`]'s wire name).
    pub kind: String,
    /// The effect class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<String>,
    /// How long it took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// `hmac-sha256:<hex>` of the subject under the kernel's key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_hash: Option<String>,
    /// Counts and flags.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub meta: Map<String, Value>,
    /// The trace or invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

impl LedgerEntry {
    /// `ts` in UTC.
    pub fn utc(&self) -> DateTime<Utc> {
        self.ts.with_timezone(&Utc)
    }

    /// The day of `ts` in its own offset: the user's local day.
    pub fn local_date(&self) -> NaiveDate {
        self.ts.date_naive()
    }

    /// The row as one line of an export, without a newline.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Serde`] when the row does not serialize, which a row the
    /// ledger accepted always does.
    pub fn to_jsonl(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

/// The per-kernel key the subject hashes are HMACs under (spec 3.2 §7.10).
///
/// It is held in the kernel home ([`FILE`](Self::FILE)), never in the ledger, so an export
/// cannot be joined to the subjects it hashes by anyone without the home. Its `Debug` shows
/// no bytes.
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::LedgerKey;
///
/// let key = LedgerKey::from_bytes([7; 32]);
/// let hash = key.hash("read_file");
/// assert!(hash.starts_with("hmac-sha256:"));
/// assert_eq!(hash, key.hash("read_file"));
/// assert_ne!(hash, LedgerKey::from_bytes([8; 32]).hash("read_file"));
/// assert_eq!(format!("{key:?}"), "LedgerKey(..)");
/// ```
#[derive(Clone)]
pub struct LedgerKey([u8; 32]);

impl fmt::Debug for LedgerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LedgerKey(..)")
    }
}

impl LedgerKey {
    /// The key's file name in the kernel home.
    pub const FILE: &'static str = "ledger.key";

    /// A key of these bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// A new random key from the operating system's generator.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] when the generator is unavailable.
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|error| KernelError::Config {
            reason: format!("the ledger key needs the operating system's random source: {error}"),
        })?;
        Ok(Self(bytes))
    }

    /// The key in the file at `path`, or a new one written there, readable only by its
    /// owner on Unix (mode `0600`). The file holds 64 hex digits. An empty file gets a new
    /// key and keeps its permissions, so a caller can create it owner-only first, as
    /// `adk-rust up` does with a protected DACL on Windows.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Io`] when the file cannot be read or created, or
    /// [`KernelError::Config`] when it does not hold a key.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::proc::LedgerKey;
    ///
    /// let home = tempfile::tempdir().unwrap();
    /// let path = home.path().join(LedgerKey::FILE);
    /// let key = LedgerKey::load_or_create(&path).unwrap();
    /// assert_eq!(key.hash("x"), LedgerKey::load_or_create(&path).unwrap().hash("x"));
    /// ```
    pub fn load_or_create(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) if text.trim().is_empty() => {
                let key = Self::generate()?;
                let mut file = std::fs::OpenOptions::new().write(true).truncate(true).open(path)?;
                use std::io::Write as _;
                writeln!(file, "{}", hex::encode(key.0))?;
                file.sync_all()?;
                return Ok(key);
            }
            Ok(text) => return Self::parse(path, &text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let key = Self::generate()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        match options.open(path) {
            Ok(mut file) => {
                use std::io::Write as _;
                writeln!(file, "{}", hex::encode(key.0))?;
                file.sync_all()?;
                Ok(key)
            }
            // Another kernel created it first: use that one.
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::parse(path, &std::fs::read_to_string(path)?)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn parse(path: &Path, text: &str) -> Result<Self> {
        let bytes =
            hex::decode(text.trim()).ok().and_then(|bytes| <[u8; 32]>::try_from(bytes).ok());
        bytes.map(Self).ok_or_else(|| KernelError::Config {
            reason: format!(
                "{} does not hold a ledger key (64 hex digits); move it aside to start a new \
                 key, which makes the existing subject hashes unrelatable to new ones",
                path.display()
            ),
        })
    }

    /// `hmac-sha256:<hex>` of `subject`.
    pub fn hash(&self, subject: &str) -> String {
        // A 32-byte key is a valid HMAC key; `new_from_slice` fails only on lengths a block
        // cipher MAC would refuse.
        let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(&self.0)
            .unwrap_or_else(|_| unreachable!("HMAC-SHA256 takes a key of any length"));
        mac.update(subject.as_bytes());
        format!("hmac-sha256:{}", hex::encode(mac.finalize().into_bytes()))
    }
}

/// How the ledger stamps a row's local offset (spec 3.2 §7.10): the day and time of day
/// the baselines read are the user's.
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::LedgerOffset;
/// use chrono::{FixedOffset, Utc};
///
/// let nairobi = FixedOffset::east_opt(3 * 3600).unwrap();
/// assert_eq!(LedgerOffset::Fixed(nairobi).offset_at(Utc::now()), nairobi);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum LedgerOffset {
    /// The operating system's local time zone at each write, daylight saving included.
    #[default]
    Local,
    /// One fixed offset.
    Fixed(FixedOffset),
}

impl LedgerOffset {
    /// The offset in force at `at`.
    pub fn offset_at(&self, at: DateTime<Utc>) -> FixedOffset {
        match self {
            Self::Local => chrono::Local.offset_from_utc_datetime(&at.naive_utc()).fix(),
            Self::Fixed(offset) => *offset,
        }
    }

    /// The local day `at` falls on.
    pub fn date_at(&self, at: DateTime<Utc>) -> NaiveDate {
        at.with_timezone(&self.offset_at(at)).date_naive()
    }
}

/// Where activity goes (spec 3.2 §7.10): the [`Ledger`], or a [`LateLedgerSink`] a runtime
/// the kernel hosts writes through before the kernel exists. The harness writes its tool
/// events and audit records through one.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::proc::{Activity, InMemoryLedgerStore, Ledger, LedgerKey, LedgerKind, LedgerSink};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let ledger = Ledger::new(Arc::new(InMemoryLedgerStore::new()), LedgerKey::from_bytes([1; 32]));
/// let sink: Arc<dyn LedgerSink> = Arc::new(ledger.clone());
/// sink.record(Activity::new(LedgerKind::Intent, "harness")).await?;
/// # Ok(())
/// # }
/// ```
#[async_trait]
pub trait LedgerSink: Send + Sync {
    /// Records `activity`.
    ///
    /// # Errors
    ///
    /// Returns an error with code [`LEDGER_INVALID_CODE`] when the activity carries
    /// something other than labels, counts and durations, or the store's error.
    async fn record(&self, activity: Activity) -> Result<()>;
}

/// A [`LedgerSink`] bound later (spec 3.2 §7.10): what a hosted runtime writes through
/// before the kernel that owns the ledger has started. Until [`bind`](Self::bind) it drops
/// activity; clones share the binding.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::proc::{Activity, InMemoryLedgerStore, LateLedgerSink, Ledger, LedgerKey, LedgerKind, LedgerSink};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let late = LateLedgerSink::new();
/// late.record(Activity::new(LedgerKind::Intent, "harness")).await?; // dropped
/// let store = Arc::new(InMemoryLedgerStore::new());
/// assert!(late.bind(Arc::new(Ledger::new(store.clone(), LedgerKey::from_bytes([1; 32])))));
/// late.record(Activity::new(LedgerKind::Intent, "harness")).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Default)]
pub struct LateLedgerSink {
    sink: Arc<OnceLock<Arc<dyn LedgerSink>>>,
}

impl fmt::Debug for LateLedgerSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LateLedgerSink").field("bound", &self.is_bound()).finish()
    }
}

impl LateLedgerSink {
    /// An unbound sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds `sink`; `false` when one is bound already, which stays.
    pub fn bind(&self, sink: Arc<dyn LedgerSink>) -> bool {
        self.sink.set(sink).is_ok()
    }

    /// Whether a sink is bound.
    pub fn is_bound(&self) -> bool {
        self.sink.get().is_some()
    }
}

#[async_trait]
impl LedgerSink for LateLedgerSink {
    async fn record(&self, activity: Activity) -> Result<()> {
        match self.sink.get() {
            Some(sink) => sink.record(activity).await,
            None => Ok(()),
        }
    }
}

/// The ledger writer (spec 3.2 §7.10): checks, hashes and stamps each [`Activity`] and
/// appends the [`LedgerEntry`] to its [`LedgerStore`]. Cloning shares it.
///
/// A `Kernel` built with `KernelBuilder::ledger` writes its events through it and reads
/// time from its own clock.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_kernel::proc::{Activity, InMemoryLedgerStore, Ledger, LedgerKey, LedgerKind, LedgerQuery, LedgerSink};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() -> adk_kernel::Result<()> {
/// let ledger = Ledger::new(Arc::new(InMemoryLedgerStore::new()), LedgerKey::from_bytes([1; 32]));
/// ledger.record(Activity::new(LedgerKind::ToolCall, "harness").with_subject("read_file")).await?;
/// let rows = ledger.store().list(&LedgerQuery::default()).await?;
/// assert_eq!(rows[0].kind, "tool_call");
/// assert!(!rows[0].to_jsonl()?.contains("read_file"));
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct Ledger {
    store: Arc<dyn LedgerStore>,
    key: Arc<LedgerKey>,
    clock: Arc<dyn Clock>,
    offset: LedgerOffset,
}

impl fmt::Debug for Ledger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ledger").field("offset", &self.offset).finish_non_exhaustive()
    }
}

impl Ledger {
    /// A ledger over `store` hashing under `key`, on the system clock and local offset.
    pub fn new(store: Arc<dyn LedgerStore>, key: LedgerKey) -> Self {
        Self {
            store,
            key: Arc::new(key),
            clock: Arc::new(SystemClock),
            offset: LedgerOffset::Local,
        }
    }

    /// Reads time from `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Stamps rows with `offset`.
    #[must_use]
    pub fn with_offset(mut self, offset: LedgerOffset) -> Self {
        self.offset = offset;
        self
    }

    /// The store rows are appended to.
    pub fn store(&self) -> &Arc<dyn LedgerStore> {
        &self.store
    }

    /// How rows are stamped.
    pub fn offset(&self) -> LedgerOffset {
        self.offset
    }

    /// The clock rows are stamped from.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// `hmac-sha256:<hex>` of `subject` under the ledger's key.
    pub fn hash(&self, subject: &str) -> String {
        self.key.hash(subject)
    }

    /// The row `activity` becomes, unstored (`seq` 0): checked, its subject hashed, stamped
    /// to the millisecond in the local offset.
    ///
    /// # Errors
    ///
    /// Returns an error with code [`LEDGER_INVALID_CODE`] when a field is not a label, a
    /// count or a duration (spec 3.2 K-I19). The message names the field, never its value.
    pub fn entry(&self, activity: Activity) -> Result<LedgerEntry> {
        let Activity { kind, agent, partition, effect, duration, subject, meta, trace_id, at } =
            activity;
        let kind = String::from(kind);
        if !is_kind(&kind) {
            return Err(invalid("kind", "a kind is 1 to 32 lowercase letters, digits and `_`"));
        }
        check_label("agent", &agent, MAX_LABEL)?;
        check_label("partition", partition.as_str(), MAX_LABEL)?;
        if let Some(effect) = &effect {
            check_label("effect", effect, MAX_EFFECT)?;
        }
        if let Some(trace_id) = &trace_id
            && !is_trace(trace_id)
        {
            return Err(invalid("trace id", "a trace id is 1 to 64 ASCII letters, digits and `-`"));
        }
        check_meta(&meta)?;
        let at = at.unwrap_or_else(|| self.clock.now());
        let at = DateTime::from_timestamp_millis(at.timestamp_millis()).unwrap_or(at);
        Ok(LedgerEntry {
            seq: 0,
            ts: at.with_timezone(&self.offset.offset_at(at)),
            partition,
            agent,
            kind,
            effect,
            duration_ms: duration
                .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)),
            subject_hash: subject.map(|subject| self.key.hash(&subject)),
            meta,
            trace_id,
        })
    }
}

#[async_trait]
impl LedgerSink for Ledger {
    async fn record(&self, activity: Activity) -> Result<()> {
        let entry = self.entry(activity)?;
        self.store.append(entry).await?;
        Ok(())
    }
}

/// Longest agent and partition label.
const MAX_LABEL: usize = 128;
/// Longest effect label.
const MAX_EFFECT: usize = 64;

/// The error refusing `field`, which never quotes the value.
fn invalid(field: &str, rule: &str) -> KernelError {
    KernelError::Core(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::InvalidInput,
        LEDGER_INVALID_CODE,
        format!(
            "the ledger refuses this {field}: {rule}. A ledger row holds labels, counts, \
             durations and hashes only, never content (spec 3.2 K-I19); pass what it is about \
             as the activity's subject, which is stored as a hash"
        ),
    ))
}

/// A kind: 1 to 32 of `[a-z0-9_]`, starting with a letter.
pub(crate) fn is_kind(value: &str) -> bool {
    let mut chars = value.chars();
    value.len() <= 32
        && chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A label: letters and digits of any script and `._:/@+-`, no space or control character.
pub(crate) fn is_label(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.chars().count() <= max
        && value.chars().all(|c| c.is_alphanumeric() || "._:/@+-".contains(c))
}

/// An identifier as a label the ledger accepts (spec 3.2 §7.10): every character a label
/// may not hold replaced with `_`, shortened to `max` characters. For identifiers a writer
/// copies into a row, such as an agent's id or a run's target — never for content, which
/// belongs in an activity's subject.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::proc::label("urn:adk:agent:my app/coder", 128), "urn:adk:agent:my_app/coder");
/// ```
pub fn label(value: &str, max: usize) -> String {
    let cleaned: String = value
        .chars()
        .take(max)
        .map(|c| if c.is_alphanumeric() || "._:/@+-".contains(c) { c } else { '_' })
        .collect();
    if cleaned.is_empty() { "_".to_string() } else { cleaned }
}

fn is_trace(value: &str) -> bool {
    (1..=64).contains(&value.len()) && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn check_label(field: &str, value: &str, max: usize) -> Result<()> {
    if is_label(value, max) {
        return Ok(());
    }
    Err(invalid(
        field,
        &format!("a label is 1 to {max} letters, digits and `._:/@+-`, with no space"),
    ))
}

fn check_meta(meta: &Map<String, Value>) -> Result<()> {
    if meta.len() > MAX_META_ENTRIES {
        return Err(invalid("meta", &format!("meta holds at most {MAX_META_ENTRIES} entries")));
    }
    for (key, value) in meta {
        if !is_kind(key) {
            return Err(invalid(
                "meta key",
                "a meta key is 1 to 32 lowercase letters, digits and `_`",
            ));
        }
        if !matches!(value, Value::Number(_) | Value::Bool(_)) {
            return Err(invalid("meta value", "a meta value is a number or a boolean"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::KernelError;

    fn ledger() -> Ledger {
        Ledger::new(
            Arc::new(super::super::InMemoryLedgerStore::new()),
            LedgerKey::from_bytes([3; 32]),
        )
        .with_offset(LedgerOffset::Fixed(FixedOffset::east_opt(3 * 3600).unwrap()))
    }

    fn code(error: KernelError) -> String {
        adk_core::AdkError::from(error).code.to_string()
    }

    #[test]
    fn an_entry_hashes_its_subject_and_stamps_the_local_offset() {
        let at = DateTime::parse_from_rfc3339("2026-10-22T16:20:04.123456Z").unwrap().to_utc();
        let entry = ledger()
            .entry(
                Activity::new(LedgerKind::ToolCall, "urn:adk:agent:adk-rust/harness")
                    .with_subject("bash\u{1f}{\"command\":\"cat .env\"}")
                    .with_duration(Duration::from_millis(1500))
                    .with_count("attempt", 2)
                    .with_flag("retried", true)
                    .with_trace_id("4bf92f3577b34da6a3ce929d0e0e4736")
                    .at(at),
            )
            .unwrap();
        assert_eq!(entry.ts.to_rfc3339(), "2026-10-22T19:20:04.123+03:00");
        assert_eq!(entry.local_date(), NaiveDate::from_ymd_opt(2026, 10, 22).unwrap());
        assert_eq!(entry.duration_ms, Some(1500));
        let hash = entry.subject_hash.clone().unwrap();
        assert_eq!(hash.len(), "hmac-sha256:".len() + 64);
        let line = entry.to_jsonl().unwrap();
        assert!(!line.contains("cat .env") && !line.contains("bash"), "{line}");
    }

    #[test]
    fn content_in_a_label_or_meta_is_refused_without_quoting_it() {
        let secret = "sk-live AKIA very secret";
        let refused = [
            Activity::new(LedgerKind::ToolCall, secret),
            Activity::new(LedgerKind::ToolCall, "harness").with_effect(secret),
            Activity::new(LedgerKind::ToolCall, "harness").with_partition(secret),
            Activity::new(LedgerKind::Other(secret.into()), "harness"),
            Activity::new(LedgerKind::ToolCall, "harness").with_trace_id(secret),
            {
                let mut activity = Activity::new(LedgerKind::ToolCall, "harness");
                activity.meta.insert("path".into(), Value::String(secret.into()));
                activity
            },
            {
                let mut activity = Activity::new(LedgerKind::ToolCall, "harness");
                activity.meta.insert("Bad Key".into(), Value::from(1));
                activity
            },
            (0..=MAX_META_ENTRIES).fold(Activity::new(LedgerKind::ToolCall, "harness"), |a, n| {
                a.with_count(format!("n{n}"), 1)
            }),
        ];
        for activity in refused {
            let error = ledger().entry(activity).unwrap_err();
            assert!(!error.to_string().contains("secret"), "{error}");
            assert_eq!(code(error), LEDGER_INVALID_CODE);
        }
    }

    #[test]
    fn labels_take_identifiers_of_any_script() {
        assert!(is_label("urn:adk:agent:adk-rust/harness", MAX_LABEL));
        assert!(is_label("ü-日本", MAX_LABEL));
        assert!(!is_label("two words", MAX_LABEL));
        assert!(!is_label("", MAX_LABEL));
        assert_eq!(label("a b\nc", 8), "a_b_c");
        assert_eq!(label("", 8), "_");
    }

    #[test]
    fn a_key_round_trips_through_its_file() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("nested").join(LedgerKey::FILE);
        let key = LedgerKey::load_or_create(&path).unwrap();
        assert_eq!(key.hash("s"), LedgerKey::load_or_create(&path).unwrap().hash("s"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the key is owner-only");
        }
        std::fs::write(&path, "not a key").unwrap();
        assert!(matches!(LedgerKey::load_or_create(&path), Err(KernelError::Config { .. })));
        // An empty file a caller created with its own permissions gets a key in place.
        std::fs::write(&path, "").unwrap();
        let filled = LedgerKey::load_or_create(&path).unwrap();
        assert_eq!(filled.hash("s"), LedgerKey::load_or_create(&path).unwrap().hash("s"));
        assert_ne!(filled.hash("s"), key.hash("s"));
    }

    #[tokio::test]
    async fn a_late_sink_drops_until_bound() {
        let store = Arc::new(super::super::InMemoryLedgerStore::new());
        let late = LateLedgerSink::new();
        late.record(Activity::new(LedgerKind::Intent, "harness")).await.unwrap();
        assert!(late.bind(Arc::new(Ledger::new(store.clone(), LedgerKey::from_bytes([1; 32])))));
        assert!(!late.bind(Arc::new(Ledger::new(store.clone(), LedgerKey::from_bytes([2; 32])))));
        late.clone().record(Activity::new(LedgerKind::Intent, "harness")).await.unwrap();
        let rows = store.list(&super::super::LedgerQuery::default()).await.unwrap();
        assert_eq!(rows.len(), 1);
    }
}
