//! The driver supervisor over `adk-tool`'s MCP manager (spec 3.2 §7.7, K-I13, K-I14).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use adk_core::audit_log::{AuditActor, AuditRecordDraft, AuditRecordKind, AuditTrail};
use adk_core::{
    ActingContext, AdkError, CredentialBroker, ErrorCategory, ErrorComponent, ReadonlyContext,
    SubjectKind, Tool, ToolOrigin, ToolPolicy, Toolset,
};
use adk_tool::mcp::{ElicitationHandler, McpServerManager, ServerStatus, ServerStatusTransition};
use adk_tool::sampling::SamplingHandler;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use super::launch::{EnforcerSource, LaunchPlan, clear_run_dir, platform_enforcer, prepare};
use super::manifest::{DriverAccess, DriverLifecycle, DriverManifest, DriverSandbox};
use super::policy::DriverPolicy;
use super::store::{DriverRecord, DriverStore};
use super::tool::{DriverElicitation, DriverToolset, LegacyWaiters};
use super::{DriverState, EffectClass, driver_of, driver_source};
use crate::audit;
use crate::bus::{EventBus, KernelEvent};
use crate::error::{KernelError, Result};
use crate::partition::{PartitionId, PartitionPolicy};

/// Builds the sampling handler of a driver whose manifest lets it sample.
pub(crate) type SamplingFactory =
    Arc<dyn Fn(&DriverManifest) -> Option<Arc<dyn SamplingHandler>> + Send + Sync>;

/// Carries a person's standing rules forward when a driver's changed declarations are
/// re-approved (spec 3.2 §7.7, K-I14).
///
/// The kernel depends on no harness type, so the harness implements this over its
/// standing-rule store (`adk-harness`'s `kernel` feature).
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::StandingRulePins;
///
/// #[derive(Debug)]
/// struct NoRules;
///
/// #[async_trait::async_trait]
/// impl StandingRulePins for NoRules {
///     async fn repin(&self, _: &str, _: &str, _: &str, _: &str, _: &str) -> adk_core::Result<usize> {
///         Ok(0)
///     }
/// }
/// ```
#[async_trait]
pub trait StandingRulePins: std::fmt::Debug + Send + Sync {
    /// Moves `subject`'s standing rules for `tool` from `source` pinned to the declaration
    /// digest `from` onto `to`, and returns how many moved.
    ///
    /// # Errors
    ///
    /// Returns the rule store's error.
    async fn repin(
        &self,
        subject: &str,
        source: &str,
        tool: &str,
        from: &str,
        to: &str,
    ) -> adk_core::Result<usize>;
}

/// What the person approving a mount sees (spec 3.2 §7.7): the command line, the
/// partitions and the effect classes, with the sandbox, its access and the secrets the
/// driver names.
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::{DriverManifest, DriverProposal, EffectClass};
///
/// let manifest = DriverManifest::from_toml(r#"
///     id = "email"
///     command = "mcp-email"
///     env = { EMAIL_TOKEN = "secret://email/token" }
///     [tools]
///     send_draft = "send_external"
/// "#).unwrap();
/// let proposal = DriverProposal::of(&manifest);
/// assert_eq!(proposal.command_line, "mcp-email");
/// assert_eq!(proposal.secrets["EMAIL_TOKEN"], "secret://email/token");
/// assert_eq!(proposal.unlisted_tools, EffectClass::Other);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DriverProposal {
    /// The driver's id.
    pub id: String,
    /// The command and its arguments, quoted as the shell would need.
    pub command_line: String,
    /// The digest recorded with the approval.
    pub command_digest: String,
    /// The partitions whose agents may use it.
    pub partitions: Vec<PartitionId>,
    /// How its process is isolated.
    pub sandbox: DriverSandbox,
    /// What a sandboxed process may reach.
    pub access: DriverAccess,
    /// The effect class of each listed tool.
    pub effect_classes: BTreeMap<String, EffectClass>,
    /// The class of every tool the manifest does not list: always `other`.
    pub unlisted_tools: EffectClass,
    /// The `secret://` references of its environment, by variable; never a secret.
    pub secrets: BTreeMap<String, String>,
    /// The labels of the data it handles.
    pub data_classes: Vec<String>,
    /// Whether it may sample the harness model.
    pub sampling: bool,
    /// When it starts.
    pub lifecycle: DriverLifecycle,
}

impl DriverProposal {
    /// The proposal of mounting `manifest`.
    pub fn of(manifest: &DriverManifest) -> Self {
        Self {
            id: manifest.id.clone(),
            command_line: manifest.command_line(),
            command_digest: manifest.command_digest(),
            partitions: manifest.partitions.clone(),
            sandbox: manifest.sandbox,
            access: manifest.access.clone(),
            effect_classes: manifest.tools.clone(),
            unlisted_tools: EffectClass::Other,
            secrets: manifest
                .secret_references()
                .into_iter()
                .map(|(name, reference)| (name, reference.to_string()))
                .collect(),
            data_classes: manifest.data_classes.clone(),
            sampling: manifest.sampling,
            lifecycle: manifest.lifecycle,
        }
    }
}

/// A mounted driver and its current state, as `GET /api/kernel/drivers` lists it.
///
/// # Example
///
/// ```rust
/// use adk_kernel::drivers::{DriverManifest, DriverRecord, DriverState, DriverView};
///
/// let manifest = DriverManifest::from_toml("id = \"fs\"\ncommand = \"mcp-fs\"\n").unwrap();
/// let view = DriverView::new(DriverRecord::new(manifest, "alice", chrono::Utc::now()), DriverState::Stopped);
/// assert_eq!(view.command_line, "mcp-fs");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct DriverView {
    /// The approved record.
    pub record: DriverRecord,
    /// The driver's state.
    pub state: DriverState,
    /// The command line that runs.
    pub command_line: String,
}

impl DriverView {
    /// The view of `record` in `state`.
    pub fn new(record: DriverRecord, state: DriverState) -> Self {
        Self { command_line: record.manifest.command_line(), record, state }
    }
}

/// Builds a [`DriverSupervisor`].
pub struct DriverSupervisorBuilder {
    store: Arc<dyn DriverStore>,
    app_name: Option<String>,
    home: Option<PathBuf>,
    broker: Option<Arc<dyn CredentialBroker>>,
    fail_if_sandbox_unavailable: bool,
    enforcer: EnforcerSource,
    sampling: Option<SamplingFactory>,
    fallback_elicitation: Option<Arc<dyn ElicitationHandler>>,
    health_interval: Duration,
    question_timeout: Duration,
    rule_pins: Option<Arc<dyn StandingRulePins>>,
    bus: Option<EventBus>,
    audit: Option<AuditTrail>,
}

impl std::fmt::Debug for DriverSupervisorBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverSupervisorBuilder")
            .field("app_name", &self.app_name)
            .field("home", &self.home)
            .field("fail_if_sandbox_unavailable", &self.fail_if_sandbox_unavailable)
            .finish_non_exhaustive()
    }
}

impl DriverSupervisorBuilder {
    /// How often a running driver's connection is checked by default: 2 seconds.
    pub const DEFAULT_HEALTH_INTERVAL: Duration = Duration::from_secs(2);
    /// How long a parked driver call's question waits by default: 24 hours.
    pub const DEFAULT_QUESTION_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

    /// Records the app's drivers in `store`; it runs under the kernel's app name and home
    /// once the kernel is built with it, unless set here.
    pub fn new(store: Arc<dyn DriverStore>) -> Self {
        Self {
            store,
            app_name: None,
            home: None,
            broker: None,
            fail_if_sandbox_unavailable: false,
            enforcer: platform_enforcer(),
            sampling: None,
            fallback_elicitation: None,
            health_interval: Self::DEFAULT_HEALTH_INTERVAL,
            question_timeout: Self::DEFAULT_QUESTION_TIMEOUT,
            rule_pins: None,
            bus: None,
            audit: None,
        }
    }

    /// The app the drivers' acting agents belong to (`urn:adk:agent:<app>/driver.<id>`).
    #[must_use]
    pub fn app_name(mut self, app_name: impl Into<String>) -> Self {
        self.app_name = Some(app_name.into());
        self
    }

    /// The kernel home; drivers keep their scratch homes and secret files under
    /// `<home>/drivers/run/<id>`.
    #[must_use]
    pub fn home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// Resolves `secret://` references (spec 3.2 §7.7). The broker is asked for each one
    /// as the driver's acting agent, `urn:adk:agent:<app>/driver.<id>`, at every start.
    #[must_use]
    pub fn credential_broker(mut self, broker: Arc<dyn CredentialBroker>) -> Self {
        self.broker = Some(broker);
        self
    }

    /// Refuses to start a driver whose sandbox is unavailable instead of recording it
    /// unsandboxed. A kernel's [`KernelConfig::fail_if_sandbox_unavailable`] also sets it.
    ///
    /// [`KernelConfig::fail_if_sandbox_unavailable`]: crate::KernelConfig::fail_if_sandbox_unavailable
    #[must_use]
    pub fn fail_if_sandbox_unavailable(mut self, fail: bool) -> Self {
        self.fail_if_sandbox_unavailable = fail;
        self
    }

    /// Builds the sampling handler of each driver whose manifest sets `sampling = true`;
    /// a driver without one cannot sample (spec 3.2 §7.7).
    #[must_use]
    pub fn sampling_handlers<F>(mut self, factory: F) -> Self
    where
        F: Fn(&DriverManifest) -> Option<Arc<dyn SamplingHandler>> + Send + Sync + 'static,
    {
        self.sampling = Some(Arc::new(factory));
        self
    }

    /// Answers elicitations a driver sends outside any call; without one they are declined.
    #[must_use]
    pub fn idle_elicitation_handler(mut self, handler: Arc<dyn ElicitationHandler>) -> Self {
        self.fallback_elicitation = Some(handler);
        self
    }

    /// How often a running driver's connection is checked; a closed one is restarted under
    /// its restart policy.
    #[must_use]
    pub fn health_check_interval(mut self, interval: Duration) -> Self {
        self.health_interval = interval;
        self
    }

    /// How long the question of a parked driver call waits for an answer.
    #[must_use]
    pub fn question_timeout(mut self, timeout: Duration) -> Self {
        self.question_timeout = timeout;
        self
    }

    /// Carries the re-approving person's standing rules forward at re-approval.
    #[must_use]
    pub fn rule_pins(mut self, pins: Arc<dyn StandingRulePins>) -> Self {
        self.rule_pins = Some(pins);
        self
    }

    /// Publishes `Driver` events on `bus`; the kernel sets its own.
    #[must_use]
    pub fn event_bus(mut self, bus: EventBus) -> Self {
        self.bus = Some(bus);
        self
    }

    /// Writes `kernel.driver.*` records to `trail`; the kernel sets its own.
    #[must_use]
    pub fn audit(mut self, trail: AuditTrail) -> Self {
        self.audit = Some(trail);
        self
    }

    /// Builds the supervisor. Nothing starts until [`DriverSupervisor::start`], which a
    /// kernel calls from `Kernel::start`.
    pub fn build(self) -> DriverSupervisor {
        DriverSupervisor {
            inner: Arc::new(Inner {
                store: self.store,
                app_name: RwLock::new(self.app_name),
                home: RwLock::new(self.home),
                broker: self.broker,
                fail_if_sandbox_unavailable: AtomicBool::new(self.fail_if_sandbox_unavailable),
                enforcer: self.enforcer,
                sampling: self.sampling,
                fallback_elicitation: self.fallback_elicitation,
                health_interval: self.health_interval,
                question_timeout: self.question_timeout,
                rule_pins: RwLock::new(self.rule_pins),
                hooks: RwLock::new(Hooks { bus: self.bus, audit: self.audit, kernel_id: None }),
                live: tokio::sync::Mutex::new(BTreeMap::new()),
                view: RwLock::new(BTreeMap::new()),
                states: RwLock::new(BTreeMap::new()),
                attempts: std::sync::Mutex::new(BTreeMap::new()),
                loaded: tokio::sync::OnceCell::new(),
                legacy: Arc::new(LegacyWaiters::default()),
                crossings: RwLock::new(BTreeMap::new()),
            }),
        }
    }
}

/// Where events and records go.
#[derive(Default)]
struct Hooks {
    bus: Option<EventBus>,
    audit: Option<AuditTrail>,
    kernel_id: Option<String>,
}

/// A mounted driver's manager while the supervisor runs.
pub(crate) struct Live {
    pub(crate) manager: Arc<McpServerManager>,
    plan: Arc<RwLock<Option<LaunchPlan>>>,
    watcher: std::sync::Mutex<Option<JoinHandle<()>>>,
    /// Held while the driver starts; `true` once its health monitor runs.
    started: tokio::sync::Mutex<bool>,
}

pub(crate) struct Inner {
    pub(crate) store: Arc<dyn DriverStore>,
    app_name: RwLock<Option<String>>,
    home: RwLock<Option<PathBuf>>,
    broker: Option<Arc<dyn CredentialBroker>>,
    fail_if_sandbox_unavailable: AtomicBool,
    enforcer: EnforcerSource,
    sampling: Option<SamplingFactory>,
    fallback_elicitation: Option<Arc<dyn ElicitationHandler>>,
    health_interval: Duration,
    pub(crate) question_timeout: Duration,
    rule_pins: RwLock<Option<Arc<dyn StandingRulePins>>>,
    hooks: RwLock<Hooks>,
    live: tokio::sync::Mutex<BTreeMap<String, Arc<Live>>>,
    /// The mounted records, read synchronously by the policy and the harness's classes.
    view: RwLock<BTreeMap<String, DriverRecord>>,
    states: RwLock<BTreeMap<String, DriverState>>,
    /// Starts since a driver last ran, for `kernel.driver.restarted`.
    attempts: std::sync::Mutex<BTreeMap<String, u32>>,
    loaded: tokio::sync::OnceCell<()>,
    pub(crate) legacy: Arc<LegacyWaiters>,
    /// The driver tool each crossing tool wraps, by driver and crossing name, so a crossing
    /// is classed and governed as the tool it reaches (spec 3.2 §7.8).
    crossings: RwLock<BTreeMap<(String, String), String>>,
}

/// The kernel's supervisor of drivers: MCP servers run as child processes under the
/// manifests the user approved (spec 3.2 §7.7).
///
/// Each mounted driver runs on an `McpServerManager` of its own, so its stdio launcher,
/// sandbox, secrets and sampling handler are its own; the manager's status channel
/// (spec 3.2 §10 T1) becomes [`KernelEvent::Driver`] events and `kernel.driver.*` records.
/// Clones share the supervisor.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_kernel::drivers::{DriverManifest, DriverSupervisor};
/// use adk_kernel::stores::InMemoryDriverStore;
///
/// # async fn example(runs: Arc<dyn adk_core::DurableRunStore>) -> adk_kernel::Result<()> {
/// let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new())).build();
/// let kernel = adk_kernel::Kernel::builder().run_store(runs).drivers(drivers.clone()).build()?;
/// kernel.start().await?;
///
/// let manifest = DriverManifest::from_file("email.toml")?;
/// println!("{:?}", adk_kernel::drivers::DriverProposal::of(&manifest)); // shown to the person
/// drivers.mount(manifest, "alice").await?;
/// let tools = drivers.toolset_for(&"work".into(), &["email".to_string()]);
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct DriverSupervisor {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for DriverSupervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mounted: Vec<String> =
            self.inner.view.read().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
        f.debug_struct("DriverSupervisor").field("mounted", &mounted).finish_non_exhaustive()
    }
}

pub(crate) fn not_mounted(id: &str) -> KernelError {
    KernelError::Core(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::NotFound,
        crate::error::NOT_FOUND_CODE,
        format!("no driver `{id}` is mounted; mount its manifest first"),
    ))
}

impl DriverSupervisor {
    /// A builder over `store`.
    pub fn builder(store: Arc<dyn DriverStore>) -> DriverSupervisorBuilder {
        DriverSupervisorBuilder::new(store)
    }

    /// The store of mounted drivers and parked calls.
    pub fn store(&self) -> &Arc<dyn DriverStore> {
        &self.inner.store
    }

    /// Sets what the kernel provides: the bus, the audit trail, the kernel's id, app, home
    /// and sandbox policy, where the builder did not.
    pub(crate) fn attach(
        &self,
        bus: &EventBus,
        audit: Option<&AuditTrail>,
        kernel_id: &str,
        app_name: &str,
        home: &Path,
        fail_if_sandbox_unavailable: bool,
    ) {
        let mut hooks = self.inner.hooks.write().unwrap_or_else(|e| e.into_inner());
        hooks.bus.get_or_insert_with(|| bus.clone());
        if hooks.audit.is_none() {
            hooks.audit = audit.cloned();
        }
        hooks.kernel_id = Some(kernel_id.to_string());
        drop(hooks);
        self.inner
            .app_name
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(|| app_name.to_string());
        self.inner
            .home
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(|| home.to_path_buf());
        if fail_if_sandbox_unavailable {
            self.inner.fail_if_sandbox_unavailable.store(true, Ordering::SeqCst);
        }
    }

    /// Sets the app and the home the drivers run under where the builder set neither, before
    /// a kernel attaches its own: a tenant kernel gives each tenant's drivers a home of the
    /// tenant's own (spec 3.2 §7.13).
    #[cfg(feature = "managed")]
    pub(crate) fn host_defaults(&self, app_name: &str, home: &Path) {
        self.inner
            .app_name
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(|| app_name.to_string());
        self.inner
            .home
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(|| home.to_path_buf());
    }

    /// Whether `other` is this supervisor, rather than a clone of another.
    #[cfg(feature = "managed")]
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Loads the mounted drivers and starts the eager ones. A driver that fails to start is
    /// logged and left `FailedToStart`; the others start.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn start(&self) -> Result<()> {
        self.inner.load().await?;
        let records: Vec<DriverRecord> =
            self.inner.view.read().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        for record in records {
            self.inner.activate(&record).await;
            if record.manifest.lifecycle == DriverLifecycle::Eager
                && let Err(error) = self.inner.ensure_started(record.id()).await
            {
                tracing::warn!(driver.id = %record.id(), error = %error, "eager driver did not start");
            }
        }
        Ok(())
    }

    /// Stops every driver's server and removes its secret files.
    pub async fn shutdown(&self) {
        let live: Vec<(String, Arc<Live>)> =
            std::mem::take(&mut *self.inner.live.lock().await).into_iter().collect();
        for (id, live) in live {
            self.inner.stop_live(&id, &live).await;
        }
    }

    /// What mounting `manifest` asks the person to approve.
    pub fn propose(&self, manifest: &DriverManifest) -> DriverProposal {
        DriverProposal::of(manifest)
    }

    /// Mounts `manifest`, approved by `by` (spec 3.2 §7.7, K-I13): records the approval as
    /// `kernel.driver.mounted` and starts an eager driver. Mounting an id again replaces
    /// the driver, its approval and its pins.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Config`] for an invalid manifest, or the store's error. An
    /// eager driver that fails to start is mounted and reported in the log and the events.
    pub async fn mount(&self, manifest: DriverManifest, by: &str) -> Result<DriverRecord> {
        manifest.validate()?;
        self.inner.load().await?;
        let mut manifest = manifest;
        manifest.declaration_digests.clear();
        let id = manifest.id.clone();
        if let Some(live) = self.inner.live.lock().await.remove(&id) {
            self.inner.stop_live(&id, &live).await;
        }
        let record = DriverRecord::new(manifest, by, Utc::now());
        self.inner.store.put(record.clone()).await?;
        self.inner.cache(&record);
        self.inner.set_state(&id, DriverState::Stopped);
        self.inner
            .record(
                audit::DRIVER_MOUNTED,
                Some(by),
                json!({
                    "driverId": id,
                    "commandDigest": record.command_digest,
                    "commandLine": record.manifest.command_line(),
                    "partitions": record.manifest.partitions,
                    "effectClasses": record.manifest.tools,
                    "sandbox": record.manifest.sandbox,
                    "digests": record.manifest.declaration_digests,
                    "by": by,
                }),
            )
            .await;
        self.inner.publish(&id, DriverState::Stopped, Some(format!("mounted by {by}")));
        self.inner.activate(&record).await;
        if record.manifest.lifecycle == DriverLifecycle::Eager
            && let Err(error) = self.inner.ensure_started(&id).await
        {
            tracing::warn!(driver.id = %id, error = %error, "eager driver did not start");
        }
        Ok(record)
    }

    /// Unmounts driver `id`: stops it, deletes its record and its parked calls, and records
    /// `kernel.driver.unmounted`. `false` when it was not mounted.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn unmount(&self, id: &str, by: &str) -> Result<bool> {
        self.inner.load().await?;
        if let Some(live) = self.inner.live.lock().await.remove(id) {
            self.inner.stop_live(id, &live).await;
        }
        let existed = self.inner.store.delete(id).await?;
        for park in self.inner.store.list_parks(Some(id)).await? {
            self.inner.store.delete_park(&park.scope, &park.call_id).await?;
        }
        self.inner.view.write().unwrap_or_else(|e| e.into_inner()).remove(id);
        self.inner.states.write().unwrap_or_else(|e| e.into_inner()).remove(id);
        if existed {
            self.inner
                .record(audit::DRIVER_UNMOUNTED, Some(by), json!({ "driverId": id, "by": by }))
                .await;
            self.inner.publish(id, DriverState::Stopped, Some(format!("unmounted by {by}")));
        }
        Ok(existed)
    }

    /// Restarts driver `id`, minting its secrets again.
    ///
    /// # Errors
    ///
    /// Returns a not-found error when `id` is not mounted, and the start's error.
    pub async fn restart(&self, id: &str) -> Result<()> {
        self.inner.load().await?;
        let live = self.inner.live_for(id).await?;
        let started = *live.started.lock().await;
        if !started {
            return self.inner.ensure_started(id).await;
        }
        self.inner.prepare_plan(id, &live).await?;
        live.manager.restart_server(id).await?;
        Ok(())
    }

    /// Re-approves driver `id` as `by` (spec 3.2 §7.7, K-I14): pins the declarations its
    /// tools publish now, clears the stale tools, moves `by`'s standing rules from the old
    /// declarations to the new, and records `kernel.driver.reapproved`.
    ///
    /// # Errors
    ///
    /// Returns a not-found error when `id` is not mounted, the start's error when it cannot
    /// run to list its tools, or the store's error.
    pub async fn reapprove(&self, id: &str, by: &str) -> Result<DriverRecord> {
        self.inner.load().await?;
        self.inner.ensure_started(id).await?;
        let observed = self.inner.declarations(id).await?;
        let mut record = self.inner.store.get(id).await?.ok_or_else(|| not_mounted(id))?;
        let previous =
            std::mem::replace(&mut record.manifest.declaration_digests, observed.clone());
        record.stale.clear();
        record.approved_by = by.to_string();
        record.approved_at = Utc::now();
        record.updated_at = record.approved_at;
        self.inner.store.put(record.clone()).await?;
        self.inner.cache(&record);
        let mut repinned = 0;
        let pins = self.inner.rule_pins.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(pins) = &pins {
            let source = driver_source(id);
            for (tool, to) in &observed {
                if let Some(from) = previous.get(tool).filter(|from| *from != to) {
                    match pins.repin(by, &source, tool, from, to).await {
                        Ok(moved) => repinned += moved,
                        Err(error) => {
                            tracing::warn!(driver.id = %id, tool = %tool, error = %error, "could not move standing rules to the re-approved declaration")
                        }
                    }
                }
            }
        }
        self.inner
            .record(
                audit::DRIVER_REAPPROVED,
                Some(by),
                json!({
                    "driverId": id,
                    "commandDigest": record.command_digest,
                    "partitions": record.manifest.partitions,
                    "effectClasses": record.manifest.tools,
                    "digests": observed,
                    "previousDigests": previous,
                    "rulesRepinned": repinned,
                    "by": by,
                }),
            )
            .await;
        self.inner.set_state(id, DriverState::Running);
        self.inner.publish(id, DriverState::Running, Some(format!("re-approved by {by}")));
        Ok(record)
    }

    /// Every mounted driver with its state, ordered by id.
    ///
    /// # Errors
    ///
    /// Returns the store's error.
    pub async fn drivers(&self) -> Result<Vec<DriverView>> {
        self.inner.load().await?;
        let states = self.inner.states.read().unwrap_or_else(|e| e.into_inner()).clone();
        Ok(self
            .inner
            .store
            .list()
            .await?
            .into_iter()
            .map(|record| {
                let state = match states.get(record.id()) {
                    Some(_) if record.needs_reapproval() => DriverState::NeedsReapproval,
                    Some(state) => *state,
                    None if record.needs_reapproval() => DriverState::NeedsReapproval,
                    None => DriverState::Stopped,
                };
                DriverView::new(record, state)
            })
            .collect())
    }

    /// The state of driver `id`, or `None` when it is not mounted.
    pub fn state(&self, id: &str) -> Option<DriverState> {
        let stale =
            self.inner.view.read().unwrap_or_else(|e| e.into_inner()).get(id)?.needs_reapproval();
        let state = self
            .inner
            .states
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .copied()
            .unwrap_or(DriverState::Stopped);
        Some(if stale { DriverState::NeedsReapproval } else { state })
    }

    /// The tools of the drivers mounted for `partition` (spec 3.2 §7.7, §7.8): a driver is
    /// served when its manifest lists `partition` and `drivers` — the partition's own list
    /// of driver ids — names it, so neither side alone widens what a partition reaches
    /// (K-I15). A lazy driver starts when the toolset is first listed.
    pub fn toolset_for(&self, partition: &PartitionId, drivers: &[String]) -> Arc<dyn Toolset> {
        Arc::new(DriverToolset::new(self.clone(), partition.clone(), Some(drivers.to_vec())))
    }

    /// The tools of every mounted driver whose manifest lists `partition`, for a kernel
    /// that keeps no partition records of its own, such as `adk-rust up` serving the
    /// default partition. Drivers mounted later are served from the next listing.
    pub fn toolset_for_manifests(&self, partition: &PartitionId) -> Arc<dyn Toolset> {
        Arc::new(DriverToolset::new(self.clone(), partition.clone(), None))
    }

    /// The drivers of `partitions` offered to the coordinator whose partition `policy`
    /// governs, as crossings (spec 3.2 §7.8, K-I15): each tool a target partition's own
    /// toolset serves — its manifest lists the partition and the partition lists the
    /// driver — named `<to>__<tool>` and registered with `policy`, so every call is the
    /// coordinator's crossing under a non-waivable approval. A read of a tool the manifest
    /// classes `read` approved once holds for the rest of the run; any other class is
    /// approved one call at a time. The partitions are read from the table at every
    /// listing: one that takes no crossing, or that the coordinator's partition blocks,
    /// offers nothing.
    ///
    /// # Example
    ///
    /// ```rust
    /// use std::sync::Arc;
    /// use adk_kernel::drivers::DriverSupervisor;
    /// use adk_kernel::stores::InMemoryDriverStore;
    /// use adk_kernel::Partitions;
    ///
    /// let drivers = DriverSupervisor::builder(Arc::new(InMemoryDriverStore::new())).build();
    /// let policy = Partitions::in_memory().policy("home").with_coordinator("harness");
    /// let crossings = drivers.crossing_toolset(&policy, ["work".into()]);
    /// assert_eq!(crossings.name(), "kernel_driver_crossings");
    /// ```
    pub fn crossing_toolset(
        &self,
        policy: &PartitionPolicy,
        partitions: impl IntoIterator<Item = PartitionId>,
    ) -> Arc<dyn Toolset> {
        Arc::new(super::tool::DriverCrossings::new(
            self.clone(),
            policy.clone(),
            partitions.into_iter().collect(),
        ))
    }

    /// Carries the re-approving person's standing rules forward at re-approval, set after
    /// the supervisor is built — for a harness built with the supervisor's tools.
    pub fn set_rule_pins(&self, pins: Arc<dyn StandingRulePins>) {
        *self.inner.rule_pins.write().unwrap_or_else(|e| e.into_inner()) = Some(pins);
    }

    /// The ids of the mounted drivers, ordered.
    pub fn mounted(&self) -> Vec<String> {
        self.inner.view.read().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect()
    }

    /// The kernel's policy layer for driver tools (spec 3.2 §7.7, K-I9, K-I14); compose it
    /// with the harness's, which it never loosens.
    pub fn policy(&self) -> Arc<dyn ToolPolicy> {
        Arc::new(DriverPolicy::new(self.clone()))
    }

    /// The effect class of `tool` when `source` is a mounted driver's
    /// ([`driver_source`]); `None` for any other source.
    pub fn class_of(&self, source: &str, tool: &str) -> Option<EffectClass> {
        let id = driver_of(source)?;
        let tool = self.wrapped_tool(id, tool).unwrap_or_else(|| tool.to_string());
        let view = self.inner.view.read().unwrap_or_else(|e| e.into_inner());
        view.get(id).map(|record| record.manifest.class_of(&tool))
    }

    /// Records that the crossing tool `crossing` wraps tool `tool` of driver `id`.
    pub(crate) fn register_crossing(&self, id: &str, crossing: &str, tool: &str) {
        self.inner
            .crossings
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert((id.to_string(), crossing.to_string()), tool.to_string());
    }

    /// The tool of driver `id` the crossing tool `crossing` wraps, if it is one.
    pub(crate) fn wrapped_tool(&self, id: &str, crossing: &str) -> Option<String> {
        self.inner
            .crossings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(id.to_string(), crossing.to_string()))
            .cloned()
    }

    /// The mounted record of driver `id`, as last read or written.
    pub fn record(&self, id: &str) -> Option<DriverRecord> {
        self.inner.view.read().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
    }

    /// The running manager of driver `id`, starting it if needed.
    pub(crate) async fn manager(&self, id: &str) -> Result<Arc<McpServerManager>> {
        self.inner.ensure_started(id).await?;
        Ok(self.inner.live_for(id).await?.manager.clone())
    }

    /// Records the declarations driver `id`'s tools publish now; see [`Inner::observe`].
    pub(crate) async fn observe(&self, id: &str, observed: BTreeMap<String, String>) {
        self.inner.observe(id, observed).await;
    }

    pub(crate) async fn load(&self) -> Result<()> {
        self.inner.load().await
    }
}

/// The digest a driver tool's declaration is pinned under.
pub(crate) fn declaration_digest(id: &str, tool: &dyn Tool) -> String {
    ToolOrigin::new(driver_source(id), &tool.declaration(), tool.annotations()).declaration_digest
}

impl Inner {
    async fn load(&self) -> Result<()> {
        self.loaded
            .get_or_try_init(|| async {
                let records = self.store.list().await?;
                let mut view = self.view.write().unwrap_or_else(|e| e.into_inner());
                for record in records {
                    view.insert(record.id().to_string(), record);
                }
                Ok::<_, KernelError>(())
            })
            .await
            .map(|_| ())
    }

    fn cache(&self, record: &DriverRecord) {
        self.view
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(record.id().to_string(), record.clone());
    }

    fn set_state(&self, id: &str, state: DriverState) {
        self.states.write().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), state);
    }

    fn publish(&self, id: &str, state: DriverState, reason: Option<String>) {
        let bus = self.hooks.read().unwrap_or_else(|e| e.into_inner()).bus.clone();
        if let Some(bus) = bus {
            bus.publish(KernelEvent::Driver { id: id.to_string(), state, reason });
        }
    }

    /// Writes a `kernel.driver.*` record (spec 3.2 §9.3), actor `by` or the kernel.
    async fn record(&self, action: &str, by: Option<&str>, fields: Value) {
        let (trail, kernel_id) = {
            let hooks = self.hooks.read().unwrap_or_else(|e| e.into_inner());
            (hooks.audit.clone(), hooks.kernel_id.clone())
        };
        let Some(trail) = trail else { return };
        let mut actor = match by {
            Some(by) => AuditActor::subject(adk_core::principal::SubjectRef::human(by)),
            None => AuditActor::default(),
        };
        actor.instance = kernel_id;
        let mut draft = AuditRecordDraft::new(
            AuditRecordKind::Other(audit::KERNEL_AUDIT_KIND.to_string()),
            action,
        )
        .actor(actor);
        if let Value::Object(fields) = fields {
            for (key, value) in fields {
                draft = draft.ext(key, value);
            }
        }
        if let Err(error) = trail.record(draft).await {
            tracing::warn!(audit.action = action, error = %error, "could not write a driver audit record");
        }
    }

    fn app_name(&self) -> String {
        self.app_name
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| "adk-rust".to_string())
    }

    fn run_dir(&self, id: &str) -> PathBuf {
        let home =
            self.home.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| {
                dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("adk-rust")
            });
        home.join("drivers").join("run").join(id)
    }

    /// The acting context driver `id` asks the credential broker as.
    fn acting(&self, id: &str) -> ActingContext {
        ActingContext::derived(&self.app_name(), &format!("driver.{id}"), "", SubjectKind::Service)
    }

    /// The live manager of a mounted driver, created on first use.
    async fn live_for(self: &Arc<Self>, id: &str) -> Result<Arc<Live>> {
        if let Some(live) = self.live.lock().await.get(id) {
            return Ok(live.clone());
        }
        let record = self.view.read().unwrap_or_else(|e| e.into_inner()).get(id).cloned();
        let record = record.ok_or_else(|| not_mounted(id))?;
        Ok(self.activate(&record).await)
    }

    /// Creates the manager of `record`'s driver and watches its status channel.
    async fn activate(self: &Arc<Self>, record: &DriverRecord) -> Arc<Live> {
        let id = record.id().to_string();
        let mut live = self.live.lock().await;
        if let Some(existing) = live.get(&id) {
            return existing.clone();
        }
        let plan: Arc<RwLock<Option<LaunchPlan>>> = Arc::new(RwLock::new(None));
        let launch_plan = plan.clone();
        let launch_id = id.clone();
        let mut manager =
            McpServerManager::new([(id.clone(), record.manifest.server.clone())].into())
                .with_name(driver_source(&id))
                .with_health_check_interval(self.health_interval)
                .with_stdio_launcher(move |config| {
                    let plan = launch_plan.read().unwrap_or_else(|e| e.into_inner()).clone();
                    match plan {
                        Some(plan) => plan.command(config),
                        None => Err(AdkError::tool(format!(
                            "driver `{launch_id}` was started without its launch plan; start it \
                             through the driver supervisor"
                        ))),
                    }
                })
                .with_server_elicitation_handler(
                    id.clone(),
                    Arc::new(DriverElicitation::new(
                        id.clone(),
                        self.legacy.clone(),
                        self.fallback_elicitation.clone(),
                    )),
                );
        if record.manifest.sampling
            && let Some(handler) =
                self.sampling.as_ref().and_then(|factory| factory(&record.manifest))
        {
            manager = manager.with_server_sampling_handler(id.clone(), handler);
        }
        let manager = Arc::new(manager);
        let transitions = manager.subscribe_status_transitions();
        let watcher = tokio::spawn(watch(Arc::downgrade(self), transitions));
        let entry = Arc::new(Live {
            manager,
            plan,
            watcher: std::sync::Mutex::new(Some(watcher)),
            started: tokio::sync::Mutex::new(false),
        });
        live.insert(id, entry.clone());
        entry
    }

    /// Prepares the launch plan of driver `id`, minting its secrets, and records how it is
    /// isolated.
    async fn prepare_plan(&self, id: &str, live: &Live) -> Result<()> {
        let record = self.store.get(id).await?.ok_or_else(|| not_mounted(id))?;
        let plan = prepare(
            &record.manifest,
            &self.run_dir(id),
            self.broker.as_deref(),
            &self.acting(id),
            self.fail_if_sandbox_unavailable.load(Ordering::SeqCst),
            &self.enforcer,
        )
        .await?;
        if record.isolation.as_ref() != Some(&plan.isolation)
            || record.process_tree != plan.process_tree
        {
            let mut record = record;
            record.isolation = Some(plan.isolation.clone());
            record.process_tree = plan.process_tree;
            record.updated_at = Utc::now();
            self.store.put(record.clone()).await?;
            self.cache(&record);
        }
        *live.plan.write().unwrap_or_else(|e| e.into_inner()) = Some(plan);
        Ok(())
    }

    /// Starts driver `id` unless it runs, and its health monitor on the first start.
    async fn ensure_started(self: &Arc<Self>, id: &str) -> Result<()> {
        self.load().await?;
        let live = self.live_for(id).await?;
        let mut started = live.started.lock().await;
        if live.manager.server_status(id).await? == ServerStatus::Running {
            return Ok(());
        }
        self.prepare_plan(id, &live).await?;
        live.manager.start_server(id).await?;
        if !*started {
            live.manager.start_monitoring();
            *started = true;
        }
        Ok(())
    }

    async fn stop_live(&self, id: &str, live: &Live) {
        live.manager.stop_monitoring();
        if let Err(error) = live.manager.shutdown().await {
            tracing::warn!(driver.id = %id, error = %error, "driver did not shut down cleanly");
        }
        if let Some(watcher) = live.watcher.lock().unwrap_or_else(|e| e.into_inner()).take() {
            watcher.abort();
        }
        clear_run_dir(&self.run_dir(id));
    }

    /// Each tool's declaration digest as driver `id` publishes it now.
    async fn declarations(&self, id: &str) -> Result<BTreeMap<String, String>> {
        let live = self.live.lock().await.get(id).cloned().ok_or_else(|| not_mounted(id))?;
        let ctx: Arc<dyn ReadonlyContext> =
            Arc::new(adk_tool::SimpleToolContext::new(format!("driver:{id}")));
        let tools = live.manager.tools(ctx).await?;
        Ok(tools
            .iter()
            .map(|tool| (tool.name().to_string(), declaration_digest(id, tool.as_ref())))
            .collect())
    }

    /// Compares the declarations driver `id` publishes with its pins (spec 3.2 §7.7, K-I14).
    ///
    /// The first listing after approval pins them. Afterwards a tool whose digest differs,
    /// or that was not there at approval, is stale: the record keeps the digest seen, the
    /// driver goes `NeedsReapproval`, and `kernel.driver.stale` is written. A tool that
    /// publishes its pinned declaration again stops being stale.
    pub(crate) async fn observe(&self, id: &str, observed: BTreeMap<String, String>) {
        let record = match self.store.get(id).await {
            Ok(Some(record)) => record,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(driver.id = %id, error = %error, "could not read a driver to check its declarations");
                return;
            }
        };
        let mut record = record;
        if record.manifest.declaration_digests.is_empty() {
            if observed.is_empty() {
                return;
            }
            record.manifest.declaration_digests = observed;
            record.updated_at = Utc::now();
            if let Err(error) = self.store.put(record.clone()).await {
                tracing::warn!(driver.id = %id, error = %error, "could not pin a driver's declarations");
                return;
            }
            self.cache(&record);
            tracing::info!(driver.id = %id, tools = record.manifest.declaration_digests.len(), "driver declarations pinned");
            return;
        }
        let pins = &record.manifest.declaration_digests;
        let stale: BTreeMap<String, String> = observed
            .iter()
            .filter(|(tool, digest)| pins.get(*tool) != Some(*digest))
            .map(|(tool, digest)| (tool.clone(), digest.clone()))
            .collect();
        if stale == record.stale {
            return;
        }
        let newly: BTreeSet<&String> =
            stale.keys().filter(|tool| !record.stale.contains_key(*tool)).collect();
        record.stale = stale.clone();
        record.updated_at = Utc::now();
        if let Err(error) = self.store.put(record.clone()).await {
            tracing::warn!(driver.id = %id, error = %error, "could not record a driver's stale tools");
            return;
        }
        self.cache(&record);
        if stale.is_empty() {
            self.publish(
                id,
                DriverState::Running,
                Some("its tools publish the approved declarations again".into()),
            );
            return;
        }
        if !newly.is_empty() {
            let tools: Vec<&String> = stale.keys().collect();
            tracing::warn!(driver.id = %id, tools = ?tools, "driver tools changed since approval; re-approval needed");
            self.record(
                audit::DRIVER_STALE,
                None,
                json!({
                    "driverId": id,
                    "commandDigest": record.command_digest,
                    "partitions": record.manifest.partitions,
                    "effectClasses": record.manifest.tools,
                    "digests": record.manifest.declaration_digests,
                    "observed": stale,
                    "tools": tools,
                }),
            )
            .await;
            self.publish(
                id,
                DriverState::NeedsReapproval,
                Some(format!(
                    "tools changed since approval: {}",
                    tools.iter().map(|tool| tool.as_str()).collect::<Vec<_>>().join(", ")
                )),
            );
        }
    }

    /// Turns one status transition into an event, a record and, on `Running`, a check of
    /// the declarations.
    async fn on_transition(self: &Arc<Self>, transition: ServerStatusTransition) {
        let ServerStatusTransition { id, from, to } = transition;
        let record = self.view.read().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
        let attempt = {
            let mut attempts = self.attempts.lock().unwrap_or_else(|e| e.into_inner());
            let count = attempts.entry(id.clone()).or_default();
            if to == ServerStatus::Restarting {
                *count += 1;
            }
            *count
        };
        let reason = match to {
            ServerStatus::Crashed => {
                Some("the server's process exited or its connection closed".to_string())
            }
            ServerStatus::Restarting => Some(format!("restart attempt {attempt}")),
            ServerStatus::FailedToStart => Some("the server did not start".to_string()),
            _ => None,
        };
        self.set_state(&id, to.into());
        let stale = record.as_ref().is_some_and(DriverRecord::needs_reapproval);
        let state = if to == ServerStatus::Running && stale {
            DriverState::NeedsReapproval
        } else {
            to.into()
        };
        self.publish(&id, state, reason);
        let fields = |record: &DriverRecord| {
            json!({
                "driverId": id,
                "commandDigest": record.command_digest,
                "partitions": record.manifest.partitions,
                "effectClasses": record.manifest.tools,
                "digests": record.manifest.declaration_digests,
                "isolation": record.isolation,
                "processTree": record.process_tree,
                "attempt": attempt,
                "from": format!("{from:?}"),
            })
        };
        if let Some(record) = &record {
            match (from, to) {
                (ServerStatus::Starting, ServerStatus::Running) if attempt == 0 => {
                    self.record(audit::DRIVER_STARTED, None, fields(record)).await;
                }
                (_, ServerStatus::Running) => {
                    self.record(audit::DRIVER_RESTARTED, None, fields(record)).await;
                }
                (_, ServerStatus::Crashed) => {
                    self.record(audit::DRIVER_CRASHED, None, fields(record)).await;
                }
                _ => {}
            }
        }
        if to == ServerStatus::Running {
            self.attempts.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), 0);
            match self.declarations(&id).await {
                Ok(observed) => self.observe(&id, observed).await,
                Err(error) => {
                    tracing::warn!(driver.id = %id, error = %error, "could not list a driver's tools to check its declarations")
                }
            }
        }
    }
}

/// Follows one driver manager's status channel until the supervisor is gone.
async fn watch(
    inner: Weak<Inner>,
    mut transitions: tokio::sync::broadcast::Receiver<ServerStatusTransition>,
) {
    loop {
        match transitions.recv().await {
            Ok(transition) => match inner.upgrade() {
                Some(inner) => inner.on_transition(transition).await,
                None => return,
            },
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(transitions.skipped = skipped, "driver status channel fell behind");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}
