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
///
/// The partitions and drivers directory of the table arrive with their phases; until then
/// they are reported as unknown keys. Flags win over both files (spec 3.2 §7.5, §7.9).
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
    /// Settings a file made that were ignored, and why.
    pub notes: Vec<String>,
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
    #[serde(flatten)]
    unknown: BTreeMap<String, toml::Value>,
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
        policy
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
            for key in user.unknown.keys() {
                settings.notes.push(format!("{origin}: unknown key `kernel.{key}` ignored"));
            }
        }
        if let Some(project) = KernelTable::load(&workspace.join(PROJECT_CONFIG_FILE))? {
            let origin = PROJECT_CONFIG_FILE;
            for (key, set) in [("home", project.home.is_some()), ("bind", project.bind.is_some())] {
                if set {
                    settings.notes.push(format!(
                        "{origin}: `kernel.{key}` ignored — where the kernel keeps its state and \
                         listens is yours to choose; set it in your user config"
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
