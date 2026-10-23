//! The `[kernel]` table of `harness.toml` (spec 3.2 §8.2): the user config sets how the
//! resident kernel runs; a workspace's `.adk/harness.toml` may only restrict it, as spec
//! 3.1 I3 requires of workspace files.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use adk_core::{AdkError, Result};
use serde::Deserialize;

use super::args::PROJECT_CONFIG_FILE;

/// How `adk-rust up` runs the kernel, from the `[kernel]` tables (spec 3.2 §8.2).
///
/// | Key | User config | Workspace `.adk/harness.toml` |
/// |---|---|---|
/// | `home` | The kernel home: its socket and `harness.db` | Ignored |
/// | `bind` | The HTTP control plane's `HOST:PORT` | Ignored |
/// | `max_concurrent_runs` | Most runs each queue's worker executes at once | Applies only when lower |
/// | `quiet_hours` | `HH:MM-HH:MM` (local) in which no proactive or maintenance process starts | A second window, applied beside the user's |
/// | `[kernel.proactivity] max_proposed_open` | Most open agenda items per partition | Applies only when lower |
/// | `[kernel.proactivity] daily_budget_usd` | The daily proactive budget's cost cap | Applies only when lower |
/// | `[kernel.proactivity] approval` | `follow_mode`, `always_ask` or `never` | Applies only when stricter |
/// | `[kernel.proactivity] review_days` | The daily review's period | Applies only when shorter |
/// | `[kernel.quotas] max_running`, `max_queued`, `daily_spend_cap_usd` | Every partition's quotas | Each applies only when lower |
/// | `[kernel.consolidation] enabled` | `false` turns the tick off | `false` turns it off |
/// | `[kernel.consolidation] min_new_sessions`, `min_new_facts`, `min_interval_minutes`, `debounce_minutes` | The tick's admission gates | Ignored, with a note |
/// | `[kernel.consolidation] max_tokens_per_tick` | The tick's token cap | Applies only when lower |
/// | `[[kernel.partitions]]`, `coordinator` | The partitions `up` runs, one harness each, and the one whose harness may cross into the others (spec 3.2 §7.8) | Ignored |
///
/// The consolidation tick is on in the CLI's Ask and Auto partitions with long-term memory
/// and off otherwise ([`enable_consolidation`](Self::enable_consolidation), spec 3.2 §13 Q1);
/// `--no-consolidation` turns it off.
///
/// A `[[kernel.partitions]]` entry has `id`, and optionally `class` (default the id),
/// `tenant`, `mode` (`read_only`, `plan`, `ask`, `auto`; default `ask`), `memory_scope`
/// (default the id), `drivers`, `quiet_hours` (`HH:MM-HH:MM`, local), `quotas` (an inline
/// table of `max_running`, `max_queued`, `daily_spend_cap_usd`) and `cross_partition`
/// (`deny`, the default, or `coordinator_with_approval`). A partition's quiet hours and
/// quotas apply beside the kernel's: every window holds, and each quota is the lower one.
///
/// The drivers directory of the table arrives with its phase; until then it is reported
/// as an unknown key. Flags win over both files (spec 3.2 §7.5, §7.8, §7.9).
///
/// # Example
///
/// ```rust
/// use adk_cli::harness::KernelSettings;
///
/// let dir = tempfile::tempdir().unwrap();
/// let user = dir.path().join("harness.toml");
/// std::fs::write(&user, "[kernel]\nbind = \"127.0.0.1:9090\"\nmax_concurrent_runs = 8\n").unwrap();
/// std::fs::create_dir(dir.path().join(".adk")).unwrap();
/// std::fs::write(dir.path().join(".adk/harness.toml"), "[kernel]\nmax_concurrent_runs = 2\nbind = \"0.0.0.0:80\"\n").unwrap();
///
/// let settings = KernelSettings::resolve(Some(&user), dir.path()).unwrap();
/// assert_eq!(settings.bind, Some("127.0.0.1:9090".parse().unwrap()));
/// assert_eq!(settings.max_concurrent_runs, Some(2));
/// assert!(settings.notes.iter().any(|note| note.contains("kernel.bind")));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct KernelSettings {
    /// The kernel home; `None` keeps `<config dir>/adk-rust`.
    pub home: Option<PathBuf>,
    /// The HTTP bind of `adk-rust up`; `None` keeps `127.0.0.1:8080`.
    pub bind: Option<SocketAddr>,
    /// Most runs each queue's worker executes at once; `None` keeps the worker default.
    pub max_concurrent_runs: Option<usize>,
    /// The user's quiet hours.
    pub quiet_hours: Option<adk_kernel::QuietHours>,
    /// The workspace's quiet hours, applied beside the user's.
    pub workspace_quiet_hours: Option<adk_kernel::QuietHours>,
    /// Most open agenda items per partition; `None` keeps 10.
    pub max_proposed_open: Option<usize>,
    /// The daily proactive budget's cost cap, in micro-USD; `None` is unlimited.
    pub daily_budget: Option<u64>,
    /// How proposals are approved; `None` follows the mode.
    pub approval: Option<adk_kernel::ProactiveApproval>,
    /// The daily review's period; `None` keeps 90 days.
    pub review_period: Option<std::time::Duration>,
    /// Every partition's quotas.
    pub quotas: adk_kernel::Quotas,
    /// Whether the consolidation tick runs: set by
    /// [`enable_consolidation`](Self::enable_consolidation).
    pub consolidation: bool,
    /// `[kernel.consolidation] enabled = false` in either file.
    pub consolidation_disabled: bool,
    /// `[kernel.consolidation]` overrides of the tick's defaults.
    pub tick: TickSettings,
    /// The partitions `up` runs, one harness each, from `[[kernel.partitions]]` in the user
    /// config (spec 3.2 §7.8); empty runs one harness, partitioned by nothing.
    pub partitions: Vec<adk_kernel::Partition>,
    /// The partition whose harness is the coordinator, from `kernel.coordinator`.
    pub coordinator: Option<adk_kernel::PartitionId>,
    /// Settings a file made that were ignored, and why.
    pub notes: Vec<String>,
}

/// One `[[kernel.partitions]]` entry as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionTable {
    id: String,
    class: Option<String>,
    tenant: Option<String>,
    mode: Option<String>,
    memory_scope: Option<String>,
    #[serde(default)]
    drivers: Vec<String>,
    quiet_hours: Option<String>,
    quotas: Option<QuotasTable>,
    cross_partition: Option<String>,
}

impl PartitionTable {
    fn partition(self, origin: &str) -> Result<adk_kernel::Partition> {
        let invalid = |error: adk_kernel::KernelError| {
            AdkError::config(format!("{origin}: partition `{}`: {error}", self.id))
        };
        let class = self.class.clone().unwrap_or_else(|| self.id.clone());
        let mut partition = adk_kernel::Partition::new(self.id.as_str(), class);
        if let Some(tenant) = &self.tenant {
            partition = partition.with_tenant(tenant.clone());
        }
        if let Some(mode) = &self.mode {
            partition = partition.with_mode(mode.parse().map_err(invalid)?);
        }
        if let Some(scope) = &self.memory_scope {
            partition = partition.with_memory_scope(scope.clone());
        }
        for driver in &self.drivers {
            partition = partition.with_driver(driver.clone());
        }
        if let Some(window) = &self.quiet_hours {
            partition = partition.with_quiet_hours(window.parse().map_err(invalid)?);
        }
        if let Some(quotas) = &self.quotas {
            let mut limits = adk_kernel::Quotas::new();
            if let Some(max) = positive(origin, quotas.max_running)? {
                limits = limits.with_max_running(max);
            }
            if let Some(max) = positive(origin, quotas.max_queued)? {
                limits = limits.with_max_queued(max);
            }
            let cap = "partitions.quotas.daily_spend_cap_usd";
            if let Some(cap) = micro_usd(origin, cap, quotas.daily_spend_cap_usd)? {
                limits = limits.with_daily_spend_cap(cap);
            }
            partition = partition.with_quotas(limits);
        }
        if let Some(cross) = &self.cross_partition {
            partition = partition.with_cross_partition(cross.parse().map_err(invalid)?);
        }
        partition.validate().map_err(invalid)?;
        Ok(partition)
    }
}

/// The `[kernel]` table as written.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct KernelTable {
    home: Option<PathBuf>,
    bind: Option<SocketAddr>,
    max_concurrent_runs: Option<usize>,
    quiet_hours: Option<String>,
    proactivity: Option<ProactivityTable>,
    quotas: Option<QuotasTable>,
    consolidation: Option<ConsolidationTable>,
    partitions: Option<Vec<PartitionTable>>,
    coordinator: Option<String>,
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
}

/// `[kernel.consolidation]` overrides of the consolidation tick's defaults (spec 3.2 §7.5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TickSettings {
    /// Distilled sessions that admit a tick; `None` keeps 3.
    pub min_new_sessions: Option<usize>,
    /// New facts that admit a tick; `None` keeps 25.
    pub min_new_facts: Option<usize>,
    /// Least time between ticks; `None` keeps 6 h.
    pub min_interval: Option<std::time::Duration>,
    /// Least idle time before a tick; `None` keeps 30 min.
    pub debounce: Option<std::time::Duration>,
    /// The tick's token cap; `None` keeps the default.
    pub max_tokens_per_tick: Option<u64>,
}

impl TickSettings {
    /// The tick with these overrides.
    pub fn tick(&self) -> adk_kernel::ConsolidationTick {
        let mut tick = adk_kernel::ConsolidationTick::default();
        if let Some(sessions) = self.min_new_sessions {
            tick = tick.with_min_new_sessions(sessions);
        }
        if let Some(facts) = self.min_new_facts {
            tick = tick.with_min_new_facts(facts);
        }
        if let Some(interval) = self.min_interval {
            tick = tick.with_min_interval(interval);
        }
        if let Some(debounce) = self.debounce {
            tick = tick.with_debounce(debounce);
        }
        if let Some(tokens) = self.max_tokens_per_tick {
            tick = tick.with_max_tokens_per_tick(tokens);
        }
        tick
    }
}

/// `[kernel.consolidation]` as written.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ConsolidationTable {
    enabled: Option<bool>,
    min_new_sessions: Option<usize>,
    min_new_facts: Option<usize>,
    min_interval_minutes: Option<u64>,
    debounce_minutes: Option<u64>,
    max_tokens_per_tick: Option<u64>,
}

/// `[kernel.proactivity]` as written.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ProactivityTable {
    max_proposed_open: Option<usize>,
    daily_budget_usd: Option<f64>,
    approval: Option<adk_kernel::ProactiveApproval>,
    review_days: Option<u64>,
}

/// `[kernel.quotas]` as written.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct QuotasTable {
    max_running: Option<usize>,
    max_queued: Option<usize>,
    daily_spend_cap_usd: Option<f64>,
}

/// Dollars as micro-USD; a negative or non-finite amount is refused.
fn micro_usd(origin: &str, key: &str, usd: Option<f64>) -> Result<Option<u64>> {
    match usd {
        None => Ok(None),
        Some(usd) => adk_core::usd_to_micro_usd(usd).map(Some).ok_or_else(|| {
            AdkError::config(format!("{origin}: `kernel.{key}` must be a positive amount of USD"))
        }),
    }
}

/// The stricter of two approvals: `never` over `always_ask` over `follow_mode`.
fn strictness(approval: adk_kernel::ProactiveApproval) -> u8 {
    match approval {
        adk_kernel::ProactiveApproval::FollowMode => 0,
        adk_kernel::ProactiveApproval::AlwaysAsk => 1,
        _ => 2,
    }
}

/// `requested` when it is lower than `current` (or `current` is unset), else `current`,
/// noting a value the workspace may not raise.
fn lower<T: PartialOrd + Copy + std::fmt::Display>(
    notes: &mut Vec<String>,
    origin: &str,
    key: &str,
    current: Option<T>,
    requested: Option<T>,
) -> Option<T> {
    match (current, requested) {
        (Some(current), Some(requested)) if requested > current => {
            notes.push(format!(
                "{origin}: `kernel.{key}` ignored — {requested} is above the {current} your user \
                 config allows"
            ));
            Some(current)
        }
        (_, Some(requested)) => Some(requested),
        (current, None) => current,
    }
}

impl KernelSettings {
    /// The proactivity policy these settings give every partition (spec 3.2 §7.5): the
    /// workspace's quiet hours as the policy's own window, beside the kernel's.
    pub fn proactivity(&self) -> adk_kernel::ProactivityPolicy {
        let mut policy = adk_kernel::ProactivityPolicy::default();
        if let Some(max) = self.max_proposed_open {
            policy = policy.with_max_proposed_open(max);
        }
        if let Some(cap) = self.daily_budget {
            policy = policy.with_daily_budget(adk_core::RunBudget::new().max_cost_micro_usd(cap));
        }
        if let Some(approval) = self.approval {
            policy = policy.with_approval(approval);
        }
        if let Some(period) = self.review_period {
            policy = policy.with_review_period(period);
        }
        if let Some(quiet) = self.workspace_quiet_hours {
            policy = policy.with_quiet_hours(quiet);
        }
        if self.consolidation {
            policy = policy.with_consolidation(self.tick.tick());
        }
        policy
    }

    /// Turns the consolidation tick on for a partition in `mode` when long-term memory and
    /// the tick are `allowed`: on in Ask and Auto, off in ReadOnly and Plan and when a file
    /// sets `enabled = false` (spec 3.2 §7.5, §13 Q1). Returns whether it is on.
    pub fn enable_consolidation(&mut self, mode: adk_harness::HarnessMode, allowed: bool) -> bool {
        self.consolidation = allowed
            && !self.consolidation_disabled
            && matches!(mode, adk_harness::HarnessMode::Ask | adk_harness::HarnessMode::Auto);
        self.consolidation
    }

    /// `config` with these settings' proactivity, quiet hours and quotas.
    pub fn apply(&self, config: adk_kernel::KernelConfig) -> adk_kernel::KernelConfig {
        let config = config.with_proactivity(self.proactivity()).with_quotas(self.quotas.clone());
        match self.quiet_hours {
            Some(quiet) => config.with_quiet_hours(quiet),
            None => config,
        }
    }
}

impl KernelTable {
    /// The `[kernel]` table of the file at `path`, if the file and the table exist.
    fn load(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AdkError::config(format!(
                    "failed to read {}: {error}",
                    path.display()
                )));
            }
        };
        let invalid = |error: &dyn std::fmt::Display| {
            AdkError::config(format!("{}: invalid [kernel] table: {error}", path.display()))
        };
        let file: toml::Table = toml::from_str(&text).map_err(|error| invalid(&error))?;
        match file.get("kernel") {
            Some(table) => table.clone().try_into().map(Some).map_err(|error| invalid(&error)),
            None => Ok(None),
        }
    }
}

impl KernelSettings {
    /// Resolves the `[kernel]` table of the user config at `user_config` (normally
    /// `<config dir>/adk-rust/harness.toml`) against the one in `workspace`'s
    /// `.adk/harness.toml`, which may only lower `max_concurrent_runs` (spec 3.2 §8.2).
    ///
    /// # Errors
    ///
    /// Returns a configuration error when a file cannot be read, its `[kernel]` table does
    /// not parse, or `max_concurrent_runs` is 0.
    pub fn resolve(user_config: Option<&Path>, workspace: &Path) -> Result<Self> {
        let mut settings = Self::default();
        if let Some(path) = user_config
            && let Some(user) = KernelTable::load(path)?
        {
            let origin = path.display().to_string();
            settings.home = user.home;
            settings.bind = user.bind;
            settings.max_concurrent_runs = positive(&origin, user.max_concurrent_runs)?;
            settings.quiet_hours = quiet(&origin, user.quiet_hours.as_deref())?;
            let proactivity = user.proactivity.unwrap_or_default();
            settings.max_proposed_open = proactivity.max_proposed_open;
            settings.daily_budget =
                micro_usd(&origin, "proactivity.daily_budget_usd", proactivity.daily_budget_usd)?;
            settings.approval = proactivity.approval;
            settings.review_period = review(&origin, proactivity.review_days)?;
            let quotas = user.quotas.unwrap_or_default();
            settings.quotas.max_running = positive(&origin, quotas.max_running)?;
            settings.quotas.max_queued = positive(&origin, quotas.max_queued)?;
            settings.quotas.daily_spend_cap =
                micro_usd(&origin, "quotas.daily_spend_cap_usd", quotas.daily_spend_cap_usd)?;
            let tick = user.consolidation.unwrap_or_default();
            settings.consolidation_disabled = tick.enabled == Some(false);
            let minutes = |value: Option<u64>| {
                value.map(|minutes| std::time::Duration::from_secs(minutes * 60))
            };
            settings.tick = TickSettings {
                min_new_sessions: tick.min_new_sessions,
                min_new_facts: tick.min_new_facts,
                min_interval: minutes(tick.min_interval_minutes),
                debounce: minutes(tick.debounce_minutes),
                max_tokens_per_tick: tick.max_tokens_per_tick,
            };
            for table in user.partitions.unwrap_or_default() {
                let partition = table.partition(&origin)?;
                if settings.partitions.iter().any(|p| p.id == partition.id) {
                    return Err(AdkError::config(format!(
                        "{origin}: partition `{}` is listed twice under [[kernel.partitions]]",
                        partition.id
                    )));
                }
                settings.partitions.push(partition);
            }
            if let Some(coordinator) = user.coordinator {
                let coordinator = adk_kernel::PartitionId::new(coordinator);
                if !settings.partitions.iter().any(|partition| partition.id == coordinator) {
                    return Err(AdkError::config(format!(
                        "{origin}: `kernel.coordinator` names `{coordinator}`, which no \
                         [[kernel.partitions]] entry defines"
                    )));
                }
                settings.coordinator = Some(coordinator);
            }
            for key in user.unknown.keys() {
                settings.notes.push(format!("{origin}: unknown key `kernel.{key}` ignored"));
            }
        }
        if let Some(project) = KernelTable::load(&workspace.join(PROJECT_CONFIG_FILE))? {
            let origin = PROJECT_CONFIG_FILE;
            for (key, set) in [
                ("home", project.home.is_some()),
                ("bind", project.bind.is_some()),
                ("partitions", project.partitions.is_some()),
                ("coordinator", project.coordinator.is_some()),
            ] {
                if set {
                    settings.notes.push(format!(
                        "{origin}: `kernel.{key}` ignored — where the kernel keeps its state, \
                         listens and draws its partitions is yours to choose; set it in your \
                         user config"
                    ));
                }
            }
            if let Some(requested) = positive(origin, project.max_concurrent_runs)? {
                match settings.max_concurrent_runs {
                    Some(current) if requested >= current => {
                        if requested > current {
                            settings.notes.push(format!(
                                "{origin}: `kernel.max_concurrent_runs` ignored — {requested} is \
                                 above the {current} your user config allows"
                            ));
                        }
                    }
                    _ => settings.max_concurrent_runs = Some(requested),
                }
            }
            // A workspace may only restrict proactivity (spec 3.2 §8.2): an extra quiet
            // window, lower limits, a stricter approval, a shorter review.
            settings.workspace_quiet_hours = quiet(origin, project.quiet_hours.as_deref())?;
            let notes = &mut settings.notes;
            let proactivity = project.proactivity.unwrap_or_default();
            settings.max_proposed_open = lower(
                notes,
                origin,
                "proactivity.max_proposed_open",
                settings.max_proposed_open,
                proactivity.max_proposed_open,
            );
            let budget =
                micro_usd(origin, "proactivity.daily_budget_usd", proactivity.daily_budget_usd)?;
            settings.daily_budget =
                lower(notes, origin, "proactivity.daily_budget_usd", settings.daily_budget, budget);
            if let Some(requested) = proactivity.approval {
                if settings
                    .approval
                    .is_none_or(|current| strictness(requested) >= strictness(current))
                {
                    settings.approval = Some(requested);
                } else {
                    notes.push(format!("{origin}: `kernel.proactivity.approval` ignored — it is looser than your user config's"));
                }
            }
            let period = review(origin, proactivity.review_days)?;
            settings.review_period = lower(
                notes,
                origin,
                "proactivity.review_days",
                settings.review_period.map(|p| p.as_secs() / 86_400),
                period.map(|p| p.as_secs() / 86_400),
            )
            .map(|days| std::time::Duration::from_secs(days * 86_400));
            let quotas = project.quotas.unwrap_or_default();
            settings.quotas.max_running = lower(
                notes,
                origin,
                "quotas.max_running",
                settings.quotas.max_running,
                positive(origin, quotas.max_running)?,
            );
            settings.quotas.max_queued = lower(
                notes,
                origin,
                "quotas.max_queued",
                settings.quotas.max_queued,
                positive(origin, quotas.max_queued)?,
            );
            let cap = micro_usd(origin, "quotas.daily_spend_cap_usd", quotas.daily_spend_cap_usd)?;
            settings.quotas.daily_spend_cap = lower(
                notes,
                origin,
                "quotas.daily_spend_cap_usd",
                settings.quotas.daily_spend_cap,
                cap,
            );
            let tick = project.consolidation.unwrap_or_default();
            if tick.enabled == Some(false) {
                settings.consolidation_disabled = true;
            }
            settings.tick.max_tokens_per_tick = lower(
                notes,
                origin,
                "consolidation.max_tokens_per_tick",
                settings.tick.max_tokens_per_tick,
                tick.max_tokens_per_tick,
            );
            for (key, set) in [
                ("min_new_sessions", tick.min_new_sessions.is_some()),
                ("min_new_facts", tick.min_new_facts.is_some()),
                ("min_interval_minutes", tick.min_interval_minutes.is_some()),
                ("debounce_minutes", tick.debounce_minutes.is_some()),
            ] {
                if set {
                    notes.push(format!(
                        "{origin}: `kernel.consolidation.{key}` ignored — when the tick runs is set in your user config"
                    ));
                }
            }
            for key in project.unknown.keys() {
                settings.notes.push(format!("{origin}: unknown key `kernel.{key}` ignored"));
            }
        }
        Ok(settings)
    }
}

fn quiet(origin: &str, text: Option<&str>) -> Result<Option<adk_kernel::QuietHours>> {
    text.map(|text| {
        text.parse::<adk_kernel::QuietHours>()
            .map_err(|error| AdkError::config(format!("{origin}: `kernel.quiet_hours`: {error}")))
    })
    .transpose()
}

fn review(origin: &str, days: Option<u64>) -> Result<Option<std::time::Duration>> {
    match days {
        Some(0) => Err(AdkError::config(format!(
            "{origin}: `kernel.proactivity.review_days` must be at least 1"
        ))),
        days => Ok(days.map(|days| std::time::Duration::from_secs(days * 86_400))),
    }
}

fn positive(origin: &str, value: Option<usize>) -> Result<Option<usize>> {
    match value {
        Some(0) => Err(AdkError::config(format!(
            "{origin}: a `kernel` count such as `max_concurrent_runs` must be at least 1"
        ))),
        value => Ok(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(user: &str, project: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let user_path = dir.path().join("user.toml");
        std::fs::write(&user_path, user).unwrap();
        std::fs::create_dir(dir.path().join(".adk")).unwrap();
        std::fs::write(dir.path().join(PROJECT_CONFIG_FILE), project).unwrap();
        (dir, user_path)
    }

    #[test]
    fn the_user_config_sets_the_kernel_and_unknown_keys_are_noted() {
        let (dir, user) = files(
            "mode = \"ask\"\n[kernel]\nhome = \"/srv/kernel\"\nbind = \"127.0.0.1:9000\"\ndrivers = \"~/.adk/drivers\"\n",
            "",
        );
        let settings = KernelSettings::resolve(Some(&user), dir.path()).unwrap();
        assert_eq!(settings.home, Some(PathBuf::from("/srv/kernel")));
        assert_eq!(settings.bind, Some("127.0.0.1:9000".parse().unwrap()));
        assert_eq!(settings.max_concurrent_runs, None);
        assert_eq!(settings.notes.len(), 1);
        assert!(settings.notes[0].contains("kernel.drivers"), "{:?}", settings.notes);
    }

    #[test]
    fn a_workspace_may_only_lower_concurrency() {
        let (dir, user) = files(
            "[kernel]\nmax_concurrent_runs = 4\n",
            "[kernel]\nmax_concurrent_runs = 9\nhome = \"/tmp/x\"\n",
        );
        let settings = KernelSettings::resolve(Some(&user), dir.path()).unwrap();
        assert_eq!((settings.max_concurrent_runs, settings.home), (Some(4), None));
        assert!(
            settings.notes.iter().any(|note| note.contains("above the 4")),
            "{:?}",
            settings.notes
        );
        assert!(
            settings.notes.iter().any(|note| note.contains("kernel.home")),
            "{:?}",
            settings.notes
        );

        let (dir, user) = files("", "[kernel]\nmax_concurrent_runs = 1\n");
        let settings = KernelSettings::resolve(Some(&user), dir.path()).unwrap();
        assert_eq!(settings.max_concurrent_runs, Some(1));
        assert!(settings.notes.is_empty());
    }

    #[test]
    fn a_workspace_may_only_restrict_proactivity_and_quotas() {
        let (dir, user) = files(
            "[kernel]\nquiet_hours = \"22:00-07:00\"\n[kernel.proactivity]\nmax_proposed_open = 5\ndaily_budget_usd = 2.0\napproval = \"always_ask\"\n[kernel.quotas]\nmax_running = 3\ndaily_spend_cap_usd = 10.0\n",
            "[kernel]\nquiet_hours = \"12:00-13:00\"\n[kernel.proactivity]\nmax_proposed_open = 20\ndaily_budget_usd = 0.5\napproval = \"follow_mode\"\nreview_days = 30\n[kernel.quotas]\nmax_running = 1\nmax_queued = 4\ndaily_spend_cap_usd = 50.0\n",
        );
        let settings = KernelSettings::resolve(Some(&user), dir.path()).unwrap();
        assert_eq!(settings.max_proposed_open, Some(5), "not raised");
        assert_eq!(settings.daily_budget, Some(500_000), "lowered");
        assert_eq!(
            settings.approval,
            Some(adk_kernel::ProactiveApproval::AlwaysAsk),
            "not loosened"
        );
        assert_eq!(settings.review_period, Some(std::time::Duration::from_secs(30 * 86_400)));
        assert_eq!(
            settings.quotas,
            adk_kernel::Quotas::new()
                .with_max_running(1)
                .with_max_queued(4)
                .with_daily_spend_cap(10_000_000)
        );
        assert!(
            settings.quiet_hours.is_some() && settings.workspace_quiet_hours.is_some(),
            "both windows hold"
        );
        for key in ["max_proposed_open", "approval", "daily_spend_cap_usd"] {
            assert!(
                settings.notes.iter().any(|note| note.contains(key)),
                "{key}: {:?}",
                settings.notes
            );
        }
        let policy = settings.proactivity();
        assert_eq!(policy.max_proposed_open, 5);
        assert_eq!(policy.daily_budget.max_cost_micro_usd, Some(500_000));
        assert_eq!(policy.quiet_hours, settings.workspace_quiet_hours);

        let (dir, user) = files("", "[kernel.proactivity]\napproval = \"never\"\n");
        let settings = KernelSettings::resolve(Some(&user), dir.path()).unwrap();
        assert_eq!(
            settings.approval,
            Some(adk_kernel::ProactiveApproval::Never),
            "proactivity off"
        );
    }

    #[test]
    fn the_tick_is_on_in_ask_and_auto_with_memory_and_a_workspace_may_turn_it_off() {
        use adk_harness::HarnessMode;
        let (dir, user) = files(
            "[kernel.consolidation]\nmin_new_facts = 1\ndebounce_minutes = 0\nmax_tokens_per_tick = 9000\n",
            "",
        );
        let mut settings = KernelSettings::resolve(Some(&user), dir.path()).unwrap();
        for (mode, allowed, on) in [
            (HarnessMode::Ask, true, true),
            (HarnessMode::Auto, true, true),
            (HarnessMode::Plan, true, false),
            (HarnessMode::ReadOnly, true, false),
            (HarnessMode::Ask, false, false),
        ] {
            assert_eq!(
                settings.enable_consolidation(mode, allowed),
                on,
                "{mode} allowed={allowed}"
            );
        }
        settings.enable_consolidation(HarnessMode::Ask, true);
        let tick = settings.proactivity().consolidation.expect("on");
        assert_eq!(
            (tick.min_new_facts, tick.debounce, tick.max_tokens_per_tick),
            (1, std::time::Duration::ZERO, 9_000)
        );
        assert_eq!(tick.min_new_sessions, 3, "unset keys keep the defaults");

        let (dir, user) =
            files("", "[kernel.consolidation]\nenabled = false\ndebounce_minutes = 0\n");
        let mut settings = KernelSettings::resolve(Some(&user), dir.path()).unwrap();
        assert!(
            !settings.enable_consolidation(HarnessMode::Auto, true),
            "the workspace turned it off"
        );
        assert!(
            settings.notes.iter().any(|note| note.contains("debounce_minutes")),
            "{:?}",
            settings.notes
        );
        assert!(settings.proactivity().consolidation.is_none());
    }

    #[test]
    fn partitions_come_from_the_user_config_only() {
        let (dir, user) = files(
            "[kernel]\ncoordinator = \"home\"\n\
             [[kernel.partitions]]\nid = \"home\"\ncross_partition = \"coordinator_with_approval\"\n\
             quiet_hours = \"22:00-07:00\"\n\
             [[kernel.partitions]]\nid = \"work\"\nclass = \"work-agents\"\nmode = \"auto\"\n\
             memory_scope = \"job\"\ndrivers = [\"calendar\"]\ncross_partition = \"coordinator_with_approval\"\n\
             quotas = { max_running = 2, daily_spend_cap_usd = 1.5 }\n",
            "[kernel]\ncoordinator = \"work\"\n[[kernel.partitions]]\nid = \"evil\"\n",
        );
        let settings = KernelSettings::resolve(Some(&user), dir.path()).unwrap();
        let expected = vec![
            adk_kernel::Partition::new("home", "home")
                .with_quiet_hours("22:00-07:00".parse().unwrap())
                .with_cross_partition(adk_kernel::CrossPartition::CoordinatorWithApproval),
            adk_kernel::Partition::new("work", "work-agents")
                .with_mode(adk_kernel::PartitionMode::Auto)
                .with_memory_scope("job")
                .with_driver("calendar")
                .with_quotas(
                    adk_kernel::Quotas::new().with_max_running(2).with_daily_spend_cap(1_500_000),
                )
                .with_cross_partition(adk_kernel::CrossPartition::CoordinatorWithApproval),
        ];
        assert_eq!(settings.partitions, expected);
        assert_eq!(settings.coordinator, Some("home".into()));
        assert!(
            settings.notes.iter().any(|note| note.contains("kernel.partitions")),
            "{:?}",
            settings.notes
        );
        assert!(
            settings.notes.iter().any(|note| note.contains("kernel.coordinator")),
            "{:?}",
            settings.notes
        );
    }

    #[test]
    fn a_bad_partition_table_is_refused() {
        for user in [
            "[kernel]\ncoordinator = \"home\"\n",
            "[[kernel.partitions]]\nid = \"home\"\n[[kernel.partitions]]\nid = \"home\"\n",
            "[[kernel.partitions]]\nid = \"home\"\nmode = \"sometimes\"\n",
            "[[kernel.partitions]]\nid = \"home\"\ncross_partition = \"maybe\"\n",
            "[[kernel.partitions]]\nid = \"\"\n",
            "[[kernel.partitions]]\nid = \"home\"\ncolour = \"blue\"\n",
            "[[kernel.partitions]]\nid = \"home\"\nquiet_hours = \"late\"\n",
            "[[kernel.partitions]]\nid = \"home\"\nquotas = { max_running = 0 }\n",
            "[[kernel.partitions]]\nid = \"home\"\nquotas = { max_parallel = 2 }\n",
        ] {
            let (dir, user_path) = files(user, "");
            assert!(KernelSettings::resolve(Some(&user_path), dir.path()).is_err(), "{user}");
        }
    }

    #[test]
    fn zero_concurrency_and_bad_tables_are_refused() {
        let (dir, user) = files("[kernel]\nmax_concurrent_runs = 0\n", "");
        assert!(KernelSettings::resolve(Some(&user), dir.path()).is_err());
        let (dir, user) = files("[kernel]\nbind = \"not an address\"\n", "");
        let error = KernelSettings::resolve(Some(&user), dir.path()).unwrap_err().to_string();
        assert!(error.contains("[kernel]"), "{error}");
        for bad in [
            "[kernel]\nquiet_hours = \"22-7\"\n",
            "[kernel.quotas]\nmax_running = 0\n",
            "[kernel.proactivity]\ndaily_budget_usd = -1.0\n",
            "[kernel.proactivity]\nreview_days = 0\n",
            "[kernel.proactivity]\nunknown = 1\n",
        ] {
            let (dir, user) = files(bad, "");
            assert!(KernelSettings::resolve(Some(&user), dir.path()).is_err(), "{bad}");
        }
    }
}
