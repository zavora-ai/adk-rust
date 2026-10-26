//! Web experiments on the site (spec 3.5 §7.12, A-I15, S11): the charter's KPIs and
//! guardrails as an `adk-eval` experiment, variants as separate static files, stable
//! assignment of an anonymous bucket, exposures and goals counted through the edge beacon, a
//! daily look at the cumulative counts, and the decision with its evidence in the weekly
//! review.
//!
//! | Part | What it does | Spec 3.5 |
//! |---|---|---|
//! | [`ExperimentPlan`] | The hypothesis, the control page, the variant's changes, the primary KPI and the guardrails, by charter id | §7.12 |
//! | [`metric_direction`], [`guardrail_spec`] | The charter's KPI direction and guardrails as the analyzer's metrics and tolerances | §7.6, §7.12 |
//! | [`assign`], [`bucket_hash`] | A stable hash of an anonymous bucket picks the arm, at the edge or in the client | §7.12 |
//! | [`ExperimentGenerator`] | Wraps the site's generator: each running experiment's variant is its own static file under `/_exp/{id}/`, and its pages load `/_awp/experiments.js` | §7.8, §7.12 |
//! | [`ExperimentRunner`] | Starts experiments, reads each day's per-arm counts, feeds the cumulative totals to `ExperimentAnalyzer` as a `Look`, keeps its `ExperimentState`, pauses the job on a guardrail breach, writes the review's evidence | §7.12, A-I15, §7.21 |
//! | [`ExperimentStore`] | Every experiment's record between looks: in memory or as JSON files | §7.12 |
//! | [`experiment_tools`] | The Growth agent's `experiment_start` and `experiment_report` over the runner | §7.12, §7.20 |
//!
//! # How an experiment runs
//!
//! 1. The Growth agent's `experiments` job writes a plan; [`ExperimentRunner::start`] maps it
//!    onto the charter in force and fixes the analysis before the first look.
//! 2. The next release carries the variant: [`ExperimentGenerator`] copies the control page,
//!    applies the plan's [`Replacement`]s, marks the copy `noindex` (its canonical link still
//!    names the control), and adds the script to both.
//! 3. In the browser the script hashes the experiment id and a random bucket kept in the
//!    tab's `sessionStorage`; the variant arm is sent to the variant's file. Each tab session
//!    sends one `exp.{id}.{arm}.exposure` event, and one `exp.{id}.{arm}.{metric}` event per
//!    goal reached (an element marked `data-awp-goal="{metric}"`). No cookie, no identifier
//!    and no address leaves the page.
//! 4. Each day [`ExperimentRunner::look`] reads the new whole days of those counts from the
//!    `beacon` KPI source, adds them to the experiment's totals and analyses the totals.
//! 5. A guardrail breach keeps the control and pauses the job through the kernel's
//!    `JobControl` before anything else (A-I15); every final decision stops the experiment,
//!    so the next build serves the control alone. The weekly review shows the decision and
//!    the analyzer's evidence block.
//!
//! The unit of analysis is a tab session's exposure: a session that reaches a goal counts as
//! one event of its arm, so both metrics are proportions of exposures.
//!
//! # Guardrails from the charter
//!
//! A charter guardrail is an absolute limit on a site metric ("unsubscribes above 0.75%");
//! an experiment guardrail limits how much worse the variant may be than the control. The
//! mapping keeps the charter's headroom: a guardrail whose metric has a baseline `b` allows
//! the variant to move the metric by at most the share that would carry the site from `b`
//! to the threshold `t` once adopted — `t/b − 1` above the baseline for a limit the metric
//! must stay under, `1 − t/b` below it for one it must stay over. A guardrail without a
//! baseline, or whose baseline already breaches it, breaches on any proven harm.
//!
//! # Example
//!
//! ```rust
//! use adk_website::experiments::{Arm, ExperimentPlan, assign};
//!
//! let plan = ExperimentPlan::new("headline", "/", "signups")
//!     .hypothesis("Saying when the bread is baked lifts sign-ups")
//!     .replace("<h1>Juniper Bakery</h1>", "<h1>Bread out of the oven at 5am</h1>")
//!     .guardrail("unsubscribes");
//! assert!(plan.validate().is_ok());
//! assert_eq!(plan.event(Arm::Variant, "newsletter.signups"), "exp.headline.b.newsletter.signups");
//! assert_eq!(plan.variant_file().unwrap(), "_exp/headline/index.html");
//!
//! // The same bucket always lands in the same arm.
//! assert_eq!(assign("headline", "9f2c41d0", 0.5), assign("headline", "9f2c41d0", 0.5));
//! ```

use std::collections::BTreeMap;

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result};
use adk_eval::experiments::{
    AnalysisMethod, DEFAULT_PLANNED_UNITS_PER_ARM, ExperimentConfig, GuardrailSpec,
    MetricDirection, MetricSpec, Tolerance,
};
use awp_types::{Breach, Charter, Direction, Guardrail};
use serde::{Deserialize, Serialize};

mod build;
mod publish;
mod runner;
mod store;
mod tools;

pub use build::ExperimentGenerator;
pub use publish::RepublishOnStop;
pub use runner::{ExperimentRunner, ExperimentStopHook, LookOutcome};
pub use store::{
    ArmCounts, Arms, ExperimentRecord, ExperimentStatus, ExperimentStore, FileExperimentStore,
    GuardedMetric, InMemoryExperimentStore,
};
pub use tools::experiment_tools;

/// The job that runs experiments (spec 3.5 §7.20).
pub const EXPERIMENTS_JOB: &str = "experiments";

/// Where the experiment script is served (spec 3.5 §7.12).
pub const SCRIPT_PATH: &str = "/_awp/experiments.js";

/// The directory every variant's files are served under: `/_exp/{id}/` (spec 3.5 §7.12).
pub const VARIANT_DIR: &str = "_exp";

/// The kernel workflow of the daily look at every running experiment (spec 3.5 §7.12).
pub const EXPERIMENTS_WORKFLOW: &str = "website.experiments";

/// The longest experiment id: an event name `exp.{id}.{arm}.{metric}` stays within the
/// beacon's 64 characters with room for the metric.
pub const MAX_ID_LEN: usize = 24;

/// The beacon's longest event name.
const MAX_EVENT_LEN: usize = 64;

/// The largest relative drop a guardrail on a metric that must stay over its threshold
/// tolerates; the analyzer refuses a drop of 100% or more.
const MAX_RELATIVE_DROP: f64 = 0.99;

/// The client script served at [`SCRIPT_PATH`].
pub const SCRIPT: &str = include_str!("experiments/experiments.js");

/// An arm of an experiment (spec 3.5 §7.12): the control (A) or the variant (B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Arm {
    /// The page as it is: A.
    Control,
    /// The page with the plan's changes: B.
    Variant,
}

impl Arm {
    /// The arm's letter in event names: `a` or `b`.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Control => "a",
            Self::Variant => "b",
        }
    }
}

/// FNV-1a over the UTF-16 code units of `key`: the hash the client script computes with
/// `charCodeAt` and `Math.imul`, so the edge and the browser assign a bucket alike (spec 3.5
/// §7.12).
///
/// # Example
///
/// ```rust
/// use adk_website::experiments::bucket_hash;
///
/// assert_eq!(bucket_hash(""), 0x811c_9dc5);
/// assert_eq!(bucket_hash("a"), 0xe40c_292c);
/// ```
pub fn bucket_hash(key: &str) -> u32 {
    key.encode_utf16()
        .fold(0x811c_9dc5_u32, |hash, unit| (hash ^ u32::from(unit)).wrapping_mul(0x0100_0193))
}

/// The arm of `bucket` in `experiment` when `allocation` of the buckets see the variant
/// (spec 3.5 §7.12): a stable hash of an anonymous bucket, so a bucket keeps its arm for the
/// experiment's life and experiments split independently.
///
/// The bucket is anonymous: the client script draws a random one per tab session and never
/// sends it; an edge that assigns uses a value it does not store.
///
/// # Example
///
/// ```rust
/// use adk_website::experiments::{Arm, assign};
///
/// assert_eq!(assign("headline", "any bucket", 0.0), Arm::Control);
/// assert_eq!(assign("headline", "any bucket", 1.0), Arm::Variant);
/// ```
pub fn assign(experiment: &str, bucket: &str, allocation: f64) -> Arm {
    let position = f64::from(bucket_hash(&format!("{experiment}:{bucket}"))) / 4_294_967_296.0;
    if position < allocation { Arm::Variant } else { Arm::Control }
}

/// The charter's KPI direction as the analyzer's metric direction (spec 3.5 §7.6, §7.12):
/// `Up` is higher-is-better and `Down` lower-is-better.
///
/// # Example
///
/// ```rust
/// use adk_eval::experiments::MetricDirection;
/// use adk_website::experiments::metric_direction;
/// use awp_types::Direction;
///
/// assert_eq!(metric_direction(Direction::Down), MetricDirection::LowerIsBetter);
/// ```
pub fn metric_direction(direction: Direction) -> MetricDirection {
    match direction {
        Direction::Down => MetricDirection::LowerIsBetter,
        _ => MetricDirection::HigherIsBetter,
    }
}

/// The direction a charter guardrail protects (spec 3.5 §7.6): a limit the metric must stay
/// under is lower-is-better; one it must stay over is higher-is-better.
pub fn guardrail_direction(breach: Breach) -> MetricDirection {
    match breach {
        Breach::Below => MetricDirection::HigherIsBetter,
        _ => MetricDirection::LowerIsBetter,
    }
}

/// How much worse than the control the variant may be on a charter guardrail's metric whose
/// baseline is `baseline` (spec 3.5 §7.12, A-I15): the share of the charter's headroom
/// between the baseline and the threshold; with no positive baseline, or none left, any
/// proven harm breaches.
///
/// # Example
///
/// ```rust
/// use std::time::Duration;
/// use adk_eval::experiments::Tolerance;
/// use adk_website::experiments::guardrail_tolerance;
/// use awp_types::{Breach, Guardrail, GuardrailAction};
///
/// let unsubscribes = Guardrail {
///     id: "unsubscribes".into(),
///     metric: "email.unsubscribes".into(),
///     threshold: 0.0075,
///     breach: Breach::Above,
///     window: Duration::from_secs(7 * 86_400),
///     action: GuardrailAction::PauseJob,
///     jobs: vec!["experiments".into()],
/// };
/// // From a 0.5% baseline the site may rise 50% before it crosses 0.75%.
/// let Tolerance::Relative(share) = guardrail_tolerance(&unsubscribes, Some(0.005)) else {
///     unreachable!()
/// };
/// assert!((share - 0.5).abs() < 1e-12);
/// assert_eq!(guardrail_tolerance(&unsubscribes, None), Tolerance::Absolute(0.0));
/// ```
pub fn guardrail_tolerance(guardrail: &Guardrail, baseline: Option<f64>) -> Tolerance {
    let Some(baseline) = baseline.filter(|value| value.is_finite() && *value > 0.0) else {
        return Tolerance::Absolute(0.0);
    };
    let ratio = guardrail.threshold / baseline;
    if !ratio.is_finite() {
        return Tolerance::Absolute(0.0);
    }
    match guardrail_direction(guardrail.breach) {
        MetricDirection::LowerIsBetter => Tolerance::Relative((ratio - 1.0).max(0.0)),
        MetricDirection::HigherIsBetter => {
            Tolerance::Relative((1.0 - ratio).clamp(0.0, MAX_RELATIVE_DROP))
        }
    }
}

/// A charter guardrail as an experiment guardrail on the proportion of exposures that reach
/// its metric (spec 3.5 §7.12, A-I15); see [`guardrail_tolerance`].
pub fn guardrail_spec(guardrail: &Guardrail, baseline: Option<f64>) -> GuardrailSpec {
    GuardrailSpec::new(
        MetricSpec::proportion(guardrail.metric.0.clone(), guardrail_direction(guardrail.breach)),
        guardrail_tolerance(guardrail, baseline),
    )
}

/// A literal change the variant makes to the control page (spec 3.5 §7.12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Replacement {
    /// HTML the control page contains.
    pub find: String,
    /// What the variant shows instead.
    pub replace: String,
}

/// An experiment the `experiments` job proposes (spec 3.5 §7.12, §7.20): one page, one
/// variant, the KPI it should move and the guardrails that stop it.
///
/// Metrics and guardrails are fixed when the experiment starts; an id is never reused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentPlan {
    /// The experiment's id: lowercase letters, digits and `-`, at most [`MAX_ID_LEN`].
    pub id: String,
    /// What the variant is expected to do, in the owner's terms.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hypothesis: String,
    /// The job that runs it, paused on a guardrail breach.
    pub job: String,
    /// The control page's URL path, such as `/` or `/pricing/`.
    pub page: String,
    /// The variant's changes to the control page.
    pub variant: Vec<Replacement>,
    /// Further pages that load the script to count goals, such as a sign-up confirmation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub goal_pages: Vec<String>,
    /// The content item rendered as the control page; [`RepublishOnStop`] republishes it
    /// when the experiment stops, so the control is served alone at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<String>,
    /// The charter KPI the variant should move.
    pub primary_kpi: String,
    /// The charter guardrails to monitor; empty means every guardrail of the job.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guardrails: Vec<String>,
    /// The share of buckets that see the variant, in `(0, 1)`.
    pub allocation: f64,
    /// How the primary metric is analysed; guardrails always use the sequential test.
    #[serde(default)]
    pub method: AnalysisMethod,
    /// The exposures per arm the experiment expects, where its boundary is tightest.
    pub planned_units_per_arm: u64,
    /// When set, the experiment stops and keeps the control at this many exposures per arm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_units_per_arm: Option<u64>,
    /// The KPI source that counts the per-arm events, `beacon` by default.
    pub source: String,
}

impl ExperimentPlan {
    /// A plan for `page` that moves the KPI `primary_kpi`, split evenly, analysed with the
    /// sequential test, counted by the beacon, run by the `experiments` job.
    pub fn new(
        id: impl Into<String>,
        page: impl Into<String>,
        primary_kpi: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            hypothesis: String::new(),
            job: EXPERIMENTS_JOB.to_string(),
            page: page.into(),
            variant: Vec::new(),
            goal_pages: Vec::new(),
            item: None,
            primary_kpi: primary_kpi.into(),
            guardrails: Vec::new(),
            allocation: 0.5,
            method: AnalysisMethod::default(),
            planned_units_per_arm: DEFAULT_PLANNED_UNITS_PER_ARM,
            max_units_per_arm: None,
            source: "beacon".to_string(),
        }
    }

    /// Sets the hypothesis.
    #[must_use]
    pub fn hypothesis(mut self, hypothesis: impl Into<String>) -> Self {
        self.hypothesis = hypothesis.into();
        self
    }

    /// Adds a change the variant makes: every `find` in the control page becomes `replace`.
    #[must_use]
    pub fn replace(mut self, find: impl Into<String>, replace: impl Into<String>) -> Self {
        self.variant.push(Replacement { find: find.into(), replace: replace.into() });
        self
    }

    /// Adds a page that loads the script only to count goals.
    #[must_use]
    pub fn goal_page(mut self, page: impl Into<String>) -> Self {
        self.goal_pages.push(page.into());
        self
    }

    /// Names the content item rendered as the control page.
    #[must_use]
    pub fn item(mut self, id: impl Into<String>) -> Self {
        self.item = Some(id.into());
        self
    }

    /// Monitors the charter guardrail `id`.
    #[must_use]
    pub fn guardrail(mut self, id: impl Into<String>) -> Self {
        self.guardrails.push(id.into());
        self
    }

    /// Sets the job that runs it.
    #[must_use]
    pub fn job(mut self, job: impl Into<String>) -> Self {
        self.job = job.into();
        self
    }

    /// Sets the share of buckets that see the variant.
    #[must_use]
    pub fn allocation(mut self, share: f64) -> Self {
        self.allocation = share;
        self
    }

    /// Sets how the primary metric is analysed.
    #[must_use]
    pub fn method(mut self, method: AnalysisMethod) -> Self {
        self.method = method;
        self
    }

    /// Sets the exposures per arm the experiment expects.
    #[must_use]
    pub fn planned_units_per_arm(mut self, units: u64) -> Self {
        self.planned_units_per_arm = units;
        self
    }

    /// Stops the experiment, keeping the control, at `units` exposures per arm.
    #[must_use]
    pub fn max_units_per_arm(mut self, units: u64) -> Self {
        self.max_units_per_arm = Some(units);
        self
    }

    /// Counts the per-arm events from the KPI source named `source`.
    #[must_use]
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// The event the client sends when a tab session of `arm` reaches `metric`.
    pub fn event(&self, arm: Arm, metric: &str) -> String {
        format!("exp.{}.{}.{metric}", self.id, arm.letter())
    }

    /// The event the client sends once per tab session shown `arm`.
    pub fn exposure_event(&self, arm: Arm) -> String {
        self.event(arm, "exposure")
    }

    /// The control page's file in the build, such as `index.html` for `/`.
    ///
    /// # Errors
    ///
    /// Returns `website.experiment.invalid` for a path that names no page file.
    pub fn control_file(&self) -> Result<String> {
        page_file(&self.id, &self.page)
    }

    /// The variant's file in the build: the control's under `_exp/{id}/`.
    ///
    /// # Errors
    ///
    /// As [`control_file`](Self::control_file).
    pub fn variant_file(&self) -> Result<String> {
        Ok(format!("{VARIANT_DIR}/{}/{}", self.id, self.control_file()?))
    }

    /// The variant's URL path: the control's under `/_exp/{id}`.
    pub fn variant_url(&self) -> String {
        format!("/{VARIANT_DIR}/{}{}", self.id, self.page)
    }

    /// Checks the plan.
    ///
    /// # Errors
    ///
    /// Returns `website.experiment.invalid` naming the first problem: an id outside
    /// `[a-z0-9-]` or longer than [`MAX_ID_LEN`], a page path that names no page file, no
    /// change or a change that finds nothing, a change that adds script, frames, embeds,
    /// forms or event handlers, an allocation outside `(0, 1)`, or an empty job or source.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || self.id.len() > MAX_ID_LEN
            || !self.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(invalid(format!(
                "experiment id '{}' must be 1 to {MAX_ID_LEN} lowercase letters, digits or '-'",
                self.id
            )));
        }
        self.control_file()?;
        for page in &self.goal_pages {
            page_file(&self.id, page)?;
        }
        if self.variant.is_empty() {
            return Err(invalid(format!(
                "experiment '{}' changes nothing; add the variant's changes with `replace`",
                self.id
            )));
        }
        for change in &self.variant {
            if change.find.is_empty() || change.find == change.replace {
                return Err(invalid(format!(
                    "experiment '{}' has a change that finds nothing or changes nothing",
                    self.id
                )));
            }
            if let Some(found) = active_content(&change.replace) {
                return Err(invalid(format!(
                    "experiment '{}' would add {found} to the variant; a variant changes text \
                     and markup only (spec 3.5 §7.12)",
                    self.id
                )));
            }
        }
        if !(self.allocation > 0.0 && self.allocation < 1.0) {
            return Err(invalid(format!(
                "experiment '{}' sends {} of buckets to the variant; use a share in (0, 1)",
                self.id, self.allocation
            )));
        }
        if self.job.trim().is_empty() || self.source.trim().is_empty() {
            return Err(invalid(format!("experiment '{}' needs a job and a source", self.id)));
        }
        Ok(())
    }

    /// The analysis this plan fixes under `charter` (spec 3.5 §7.12): the primary KPI's metric
    /// in its direction and every guardrail with its tolerance from `baselines` (metric →
    /// baseline value), and the guardrails it monitors.
    ///
    /// # Errors
    ///
    /// Returns `website.experiment.invalid` for a KPI or guardrail the charter does not
    /// have, a metric whose event name would not fit the beacon, or a configuration the
    /// analyzer refuses.
    pub fn config(
        &self,
        charter: &Charter,
        baselines: &BTreeMap<String, f64>,
    ) -> Result<(ExperimentConfig, Vec<GuardedMetric>)> {
        self.validate()?;
        let Some(kpi) = charter.kpis.iter().find(|kpi| kpi.id == self.primary_kpi) else {
            let known: Vec<&str> = charter.kpis.iter().map(|kpi| kpi.id.as_str()).collect();
            return Err(invalid(format!(
                "charter version {} has no KPI '{}'; name one of {known:?}",
                charter.version, self.primary_kpi
            )));
        };
        let primary = kpi.metric.0.clone();
        self.check_metric(&primary)?;
        let mut config = ExperimentConfig::new(
            self.id.clone(),
            MetricSpec::proportion(primary.clone(), metric_direction(kpi.direction)),
        )
        .with_method(self.method)
        .with_planned_units_per_arm(self.planned_units_per_arm);
        if let Some(max) = self.max_units_per_arm {
            config = config.with_max_units_per_arm(max);
        }
        let chosen: Vec<&Guardrail> = if self.guardrails.is_empty() {
            charter
                .guardrails
                .iter()
                .filter(|guardrail| guardrail.jobs.is_empty() || guardrail.jobs.contains(&self.job))
                .collect()
        } else {
            let mut chosen = Vec::new();
            for id in &self.guardrails {
                let Some(guardrail) = charter.guardrails.iter().find(|g| &g.id == id) else {
                    return Err(invalid(format!(
                        "charter version {} has no guardrail '{id}'",
                        charter.version
                    )));
                };
                chosen.push(guardrail);
            }
            chosen
        };
        let mut guarded = Vec::new();
        for guardrail in chosen {
            let metric = guardrail.metric.0.clone();
            if metric == primary {
                return Err(invalid(format!(
                    "guardrail '{}' watches the primary metric '{metric}'; an experiment cannot \
                     guard the metric it tries to move",
                    guardrail.id
                )));
            }
            if guarded.iter().any(|g: &GuardedMetric| g.metric == metric) {
                continue;
            }
            self.check_metric(&metric)?;
            let spec = guardrail_spec(guardrail, baselines.get(&metric).copied());
            guarded.push(GuardedMetric {
                guardrail: guardrail.id.clone(),
                metric,
                threshold: guardrail.threshold,
                tolerance: spec.tolerance,
            });
            config = config.with_guardrail(spec);
        }
        config.validate().map_err(|error| invalid(error.to_string()))?;
        Ok((config, guarded))
    }

    /// Refuses a metric whose event name the beacon would refuse.
    fn check_metric(&self, metric: &str) -> Result<()> {
        let event = self.event(Arm::Control, metric);
        let valid = metric != "exposure"
            && event.len() <= MAX_EVENT_LEN
            && metric
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
        if valid {
            Ok(())
        } else {
            Err(invalid(format!(
                "metric '{metric}' cannot be counted per arm: its event '{event}' must be at most \
                 {MAX_EVENT_LEN} lowercase letters, digits, '.', '_' or '-', and not 'exposure'; \
                 shorten the experiment id or rename the metric"
            )))
        }
    }
}

/// The build file of page `path`: `index.html` under a directory path, the path itself for
/// a `.html` file.
fn page_file(id: &str, path: &str) -> Result<String> {
    let relative = path.strip_prefix('/').ok_or_else(|| {
        invalid(format!("experiment '{id}': page '{path}' must be a URL path starting with '/'"))
    })?;
    let clean = !relative.split('/').any(|segment| segment == ".." || segment == ".")
        && !relative.contains("//")
        && relative.bytes().all(|b| b.is_ascii_graphic() && !b"\"'<>\\?#&%".contains(&b));
    if !clean {
        return Err(invalid(format!(
            "experiment '{id}': page '{path}' must be a plain path without queries, fragments or \
             dot segments"
        )));
    }
    if relative.is_empty() || relative.ends_with('/') {
        Ok(format!("{relative}index.html"))
    } else if relative.ends_with(".html") {
        Ok(relative.to_string())
    } else {
        Err(invalid(format!(
            "experiment '{id}': page '{path}' names no page file; end it with '/' or '.html'"
        )))
    }
}

/// The first element or attribute in `html` that loads or runs code.
fn active_content(html: &str) -> Option<&'static str> {
    let lower = html.to_ascii_lowercase();
    for (element, name) in [
        ("<script", "a script"),
        ("<iframe", "a frame"),
        ("<frame", "a frame"),
        ("<object", "an embedded object"),
        ("<embed", "an embedded object"),
        ("<link", "a linked resource"),
        ("<meta", "a meta element"),
        ("<base", "a base element"),
        ("<form", "a form"),
        ("<style", "a style sheet"),
    ] {
        if lower.contains(element) {
            return Some(name);
        }
    }
    // Attributes are inside tags; text outside them may say anything.
    let mut rest = lower.as_str();
    while let Some(start) = rest.find('<') {
        let after = &rest[start + 1..];
        let end = after.find('>').unwrap_or(after.len());
        let tag = &after[..end];
        if ["javascript:", "vbscript:", "data:"].iter().any(|scheme| tag.contains(scheme)) {
            return Some("a script URL");
        }
        if has_event_handler(tag) {
            return Some("an event handler");
        }
        rest = &after[end..];
    }
    None
}

/// Whether the inside of a tag has an `on…=` attribute.
fn has_event_handler(tag: &str) -> bool {
    let bytes = tag.as_bytes();
    (1..bytes.len()).any(|index| {
        let boundary =
            matches!(bytes[index - 1], b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'"' | b'\'');
        if !boundary || !bytes[index..].starts_with(b"on") {
            return false;
        }
        let letters = bytes[index + 2..].iter().take_while(|b| b.is_ascii_lowercase()).count();
        letters > 0 && bytes[index + 2 + letters..].trim_ascii_start().starts_with(b"=")
    })
}

/// A refused plan or look.
pub(crate) fn invalid(message: impl Into<String>) -> AdkError {
    AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::InvalidInput,
        "website.experiment.invalid",
        message.into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_matches_fnv_1a_test_vectors() {
        // FNV-1a 32-bit vectors from the reference implementation (ASCII, one unit a byte).
        assert_eq!(bucket_hash(""), 0x811c_9dc5);
        assert_eq!(bucket_hash("a"), 0xe40c_292c);
        assert_eq!(bucket_hash("foobar"), 0xbf9c_f968);
    }

    #[test]
    fn pages_map_to_their_files() {
        let plan = |page: &str| ExperimentPlan::new("h", page, "k").replace("a", "b");
        assert_eq!(plan("/").control_file().unwrap(), "index.html");
        assert_eq!(plan("/pricing/").control_file().unwrap(), "pricing/index.html");
        assert_eq!(plan("/about.html").variant_file().unwrap(), "_exp/h/about.html");
        assert_eq!(plan("/pricing/").variant_url(), "/_exp/h/pricing/");
        for bad in ["pricing/", "/pricing", "/a/../b/", "/a?b=1", "/\"x/"] {
            assert!(plan(bad).validate().is_err(), "{bad}");
        }
    }

    #[test]
    fn a_variant_adds_no_active_content() {
        for html in [
            "<script>x()</script>",
            "<img src=x onerror=alert(1)>",
            "<a href=\"javascript:x()\">",
            "<IFRAME src=x>",
            "<p ONCLICK = \"x\">",
            "<form action=/x>",
        ] {
            assert!(active_content(html).is_some(), "{html}");
        }
        for html in ["<h1>Bread at 5am</h1>", "<p>Join 2,000 readers on Monday</p>", "only"] {
            assert_eq!(active_content(html), None, "{html}");
        }
    }
}
