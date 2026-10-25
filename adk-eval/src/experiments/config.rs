//! Experiment configuration: metrics, guardrails, the error budget and the analysis method
//! (spec 3.5 §7.12, §13 Q8).

use serde::{Deserialize, Serialize};

use crate::error::{EvalError, Result};

/// Default type I error budget of the primary metric's two-sided test (spec 3.5 §7.12).
pub const DEFAULT_ALPHA: f64 = 0.05;

/// Default error budget shared by all guardrails of an experiment (spec 3.5 §7.12).
///
/// A false guardrail stop keeps the control and costs only the experiment, so guardrails get a
/// larger budget than the primary metric and detect harm sooner.
pub const DEFAULT_GUARDRAIL_ALPHA: f64 = 0.10;

/// Default planned units per arm, where the sequential boundary is tightest (spec 3.5 §7.12).
pub const DEFAULT_PLANNED_UNITS_PER_ARM: u64 = 10_000;

/// Default units each arm needs before a metric's test may decide (spec 3.5 §7.12).
pub const DEFAULT_MIN_UNITS_PER_ARM: u64 = 100;

/// How a metric is measured per unit (spec 3.5 §7.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum MetricKind {
    /// The share of units with at least one event: sign-ups, unsubscribes, orders.
    Proportion,
    /// The mean of a per-unit value: revenue per visitor, pages per session.
    Mean,
}

/// Which way a metric improves (spec 3.5 §7.12).
///
/// The charter's KPI direction `Up` maps to [`MetricDirection::HigherIsBetter`] and `Down` to
/// [`MetricDirection::LowerIsBetter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MetricDirection {
    /// Larger values are better: sign-ups, revenue.
    HigherIsBetter,
    /// Smaller values are better: unsubscribes, refunds, errors.
    LowerIsBetter,
}

impl MetricDirection {
    /// Returns `+1.0` when higher is better and `-1.0` when lower is better.
    pub(crate) fn sign(self) -> f64 {
        match self {
            Self::HigherIsBetter => 1.0,
            Self::LowerIsBetter => -1.0,
        }
    }
}

/// A metric an experiment measures in both arms (spec 3.5 §7.12).
///
/// # Example
///
/// ```
/// use adk_eval::experiments::{MetricDirection, MetricKind, MetricSpec};
///
/// let signups = MetricSpec::proportion("signups", MetricDirection::HigherIsBetter);
/// assert_eq!(signups.kind, MetricKind::Proportion);
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct MetricSpec {
    /// The metric's id; each [`Look`](super::Look) carries samples under this id.
    pub id: String,
    /// How the metric is measured per unit.
    pub kind: MetricKind,
    /// Which way the metric improves.
    pub direction: MetricDirection,
}

impl MetricSpec {
    /// Creates a metric of the given kind.
    pub fn new(id: impl Into<String>, kind: MetricKind, direction: MetricDirection) -> Self {
        Self { id: id.into(), kind, direction }
    }

    /// Creates a proportion metric: the share of units with an event.
    pub fn proportion(id: impl Into<String>, direction: MetricDirection) -> Self {
        Self::new(id, MetricKind::Proportion, direction)
    }

    /// Creates a mean metric: the average of a per-unit value.
    pub fn mean(id: impl Into<String>, direction: MetricDirection) -> Self {
        Self::new(id, MetricKind::Mean, direction)
    }
}

/// How much worse than the control a guardrail metric may get before it is breached
/// (spec 3.5 §7.12).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum Tolerance {
    /// An absolute margin in the metric's units: `Absolute(0.002)` tolerates 0.2 percentage
    /// points more unsubscribes. `Absolute(0.0)` breaches on any proven harm.
    Absolute(f64),
    /// A margin relative to the control: `Relative(0.1)` tolerates the variant being up to 10%
    /// worse than the control.
    Relative(f64),
}

/// A guardrail: a metric the variant must not make worse beyond a tolerance (spec 3.5 §7.12,
/// A-I15).
///
/// # Example
///
/// ```
/// use adk_eval::experiments::{GuardrailSpec, MetricDirection, MetricSpec, Tolerance};
///
/// // Unsubscribes may rise by at most 10% relative to the control.
/// let guardrail = GuardrailSpec::new(
///     MetricSpec::proportion("unsubscribes", MetricDirection::LowerIsBetter),
///     Tolerance::Relative(0.1),
/// );
/// assert_eq!(guardrail.metric.id, "unsubscribes");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct GuardrailSpec {
    /// The guarded metric.
    pub metric: MetricSpec,
    /// How much worse than the control the variant may be.
    pub tolerance: Tolerance,
}

impl GuardrailSpec {
    /// Creates a guardrail on `metric` with `tolerance`.
    pub fn new(metric: MetricSpec, tolerance: Tolerance) -> Self {
        Self { metric, tolerance }
    }
}

/// The fixed type I error budget of an experiment (spec 3.5 §7.12).
///
/// `alpha` bounds the probability that the primary test ever declares a difference when there
/// is none, however many looks are taken. `guardrail_alpha` bounds the probability that any
/// guardrail is ever breached when the variant is within every tolerance; it is split equally
/// across the guardrails.
///
/// # Example
///
/// ```
/// use adk_eval::experiments::ErrorBudget;
///
/// let budget = ErrorBudget::new(0.05, 0.10);
/// assert_eq!(budget, ErrorBudget::default());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ErrorBudget {
    /// The primary metric's two-sided type I error budget.
    pub alpha: f64,
    /// The type I error budget shared by all guardrails.
    pub guardrail_alpha: f64,
}

impl ErrorBudget {
    /// Creates a budget with `alpha` for the primary metric and `guardrail_alpha` shared by the
    /// guardrails.
    pub fn new(alpha: f64, guardrail_alpha: f64) -> Self {
        Self { alpha, guardrail_alpha }
    }
}

impl Default for ErrorBudget {
    fn default() -> Self {
        Self::new(DEFAULT_ALPHA, DEFAULT_GUARDRAIL_ALPHA)
    }
}

/// A Beta prior for proportion metrics in the Bayesian analysis (spec 3.5 §13 Q8).
///
/// The default `Beta(1, 1)` is uniform on the rate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct BetaPrior {
    /// Prior pseudo-count of units with an event.
    pub alpha: f64,
    /// Prior pseudo-count of units without an event.
    pub beta: f64,
}

impl BetaPrior {
    /// Creates a `Beta(alpha, beta)` prior.
    pub fn new(alpha: f64, beta: f64) -> Self {
        Self { alpha, beta }
    }
}

impl Default for BetaPrior {
    fn default() -> Self {
        Self::new(1.0, 1.0)
    }
}

/// Settings of the Bayesian analysis of the primary metric (spec 3.5 §13 Q8).
///
/// # Example
///
/// ```
/// use adk_eval::experiments::BayesianConfig;
///
/// let config = BayesianConfig::default().with_max_expected_loss(0.001);
/// assert_eq!(config.decision_probability, 0.95);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct BayesianConfig {
    /// The prior of each arm's rate for proportion metrics. Mean metrics use a flat prior.
    pub prior: BetaPrior,
    /// The posterior probability that one arm is better needed to decide (default 0.95).
    pub decision_probability: f64,
    /// When set, a decision also needs the expected loss of the chosen arm, in the metric's
    /// units, to be at most this value.
    pub max_expected_loss: Option<f64>,
    /// The level of the reported credible intervals (default 0.95).
    pub credible_level: f64,
}

impl BayesianConfig {
    /// Sets the prior of each arm's rate for proportion metrics.
    pub fn with_prior(mut self, prior: BetaPrior) -> Self {
        self.prior = prior;
        self
    }

    /// Sets the posterior probability needed to decide.
    pub fn with_decision_probability(mut self, probability: f64) -> Self {
        self.decision_probability = probability;
        self
    }

    /// Requires the chosen arm's expected loss to be at most `loss` before deciding.
    pub fn with_max_expected_loss(mut self, loss: f64) -> Self {
        self.max_expected_loss = Some(loss);
        self
    }

    /// Sets the level of the reported credible intervals.
    pub fn with_credible_level(mut self, level: f64) -> Self {
        self.credible_level = level;
        self
    }
}

impl Default for BayesianConfig {
    fn default() -> Self {
        Self {
            prior: BetaPrior::default(),
            decision_probability: 0.95,
            max_expected_loss: None,
            credible_level: 0.95,
        }
    }
}

/// How the primary metric is analysed (spec 3.5 §7.12, §13 Q8).
///
/// Guardrails always use the sequential test, whatever the primary metric's method: a breach
/// is a safety stop and does not depend on a prior.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum AnalysisMethod {
    /// The always-valid sequential test with a fixed error budget (the default).
    #[default]
    Sequential,
    /// Posterior probabilities, credible intervals and expected loss. It has no type I error
    /// guarantee when looked at repeatedly.
    Bayesian(BayesianConfig),
}

/// The configuration of one experiment: a control, one variant, a primary metric, guardrails,
/// the error budget and the analysis method (spec 3.5 §7.12).
///
/// # Example
///
/// ```
/// use adk_eval::experiments::{
///     ExperimentConfig, GuardrailSpec, MetricDirection, MetricSpec, Tolerance,
/// };
///
/// let config = ExperimentConfig::new(
///     "headline",
///     MetricSpec::proportion("signups", MetricDirection::HigherIsBetter),
/// )
/// .with_guardrail(GuardrailSpec::new(
///     MetricSpec::proportion("unsubscribes", MetricDirection::LowerIsBetter),
///     Tolerance::Relative(0.1),
/// ))
/// .with_planned_units_per_arm(20_000);
/// assert!(config.validate().is_ok());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ExperimentConfig {
    /// The experiment's id.
    pub id: String,
    /// The metric the experiment tries to improve.
    pub primary: MetricSpec,
    /// The metrics the variant must not make worse (A-I15).
    pub guardrails: Vec<GuardrailSpec>,
    /// The fixed type I error budget.
    pub budget: ErrorBudget,
    /// How the primary metric is analysed.
    pub method: AnalysisMethod,
    /// The units per arm the experiment expects to reach; the sequential boundary is tightest
    /// there. It tunes power, not validity.
    pub planned_units_per_arm: u64,
    /// The units each arm of a metric needs before that metric's test may decide.
    pub min_units_per_arm: u64,
    /// When set, the experiment stops and keeps the control once both arms of the primary
    /// metric reach this many units without a decision.
    pub max_units_per_arm: Option<u64>,
}

impl ExperimentConfig {
    /// Creates a sequential experiment with no guardrails and the default budget and sizes.
    pub fn new(id: impl Into<String>, primary: MetricSpec) -> Self {
        Self {
            id: id.into(),
            primary,
            guardrails: Vec::new(),
            budget: ErrorBudget::default(),
            method: AnalysisMethod::default(),
            planned_units_per_arm: DEFAULT_PLANNED_UNITS_PER_ARM,
            min_units_per_arm: DEFAULT_MIN_UNITS_PER_ARM,
            max_units_per_arm: None,
        }
    }

    /// Adds a guardrail.
    pub fn with_guardrail(mut self, guardrail: GuardrailSpec) -> Self {
        self.guardrails.push(guardrail);
        self
    }

    /// Sets the error budget.
    pub fn with_budget(mut self, budget: ErrorBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Sets the analysis method of the primary metric.
    pub fn with_method(mut self, method: AnalysisMethod) -> Self {
        self.method = method;
        self
    }

    /// Sets the units per arm the experiment expects to reach.
    pub fn with_planned_units_per_arm(mut self, units: u64) -> Self {
        self.planned_units_per_arm = units;
        self
    }

    /// Sets the units each arm needs before a metric's test may decide.
    pub fn with_min_units_per_arm(mut self, units: u64) -> Self {
        self.min_units_per_arm = units;
        self
    }

    /// Stops the experiment, keeping the control, once both arms reach `units` undecided.
    pub fn with_max_units_per_arm(mut self, units: u64) -> Self {
        self.max_units_per_arm = Some(units);
        self
    }

    /// Checks the configuration.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::ConfigError`] naming the first invalid setting: an empty id, a
    /// repeated metric id, an error budget outside `(0, 0.5]`, a negative or unusable
    /// tolerance, sizes that are zero or out of order, or Bayesian settings out of range.
    pub fn validate(&self) -> Result<()> {
        let invalid = |message: String| Err(EvalError::ConfigError(message));
        if self.id.trim().is_empty() {
            return invalid("experiment id is empty; give the experiment a stable id".into());
        }
        let mut ids = vec![self.primary.id.as_str()];
        for guardrail in &self.guardrails {
            ids.push(guardrail.metric.id.as_str());
        }
        for (index, id) in ids.iter().enumerate() {
            if id.trim().is_empty() {
                return invalid(format!("experiment '{}' has a metric with an empty id", self.id));
            }
            if ids[..index].contains(id) {
                return invalid(format!(
                    "experiment '{}' names metric '{id}' twice; the primary metric and each \
                     guardrail need distinct metric ids",
                    self.id
                ));
            }
        }
        for (name, value) in
            [("alpha", self.budget.alpha), ("guardrail_alpha", self.budget.guardrail_alpha)]
        {
            if !(value > 0.0 && value <= 0.5) {
                return invalid(format!(
                    "experiment '{}' has {name} = {value}; an error budget must lie in (0, 0.5]",
                    self.id
                ));
            }
        }
        for guardrail in &self.guardrails {
            let metric = &guardrail.metric.id;
            match guardrail.tolerance {
                Tolerance::Absolute(margin) => {
                    if !(margin.is_finite() && margin >= 0.0) {
                        return invalid(format!(
                            "guardrail '{metric}' has absolute tolerance {margin}; use a finite \
                             margin of zero or more"
                        ));
                    }
                    if guardrail.metric.kind == MetricKind::Proportion && margin >= 1.0 {
                        return invalid(format!(
                            "guardrail '{metric}' tolerates an absolute change of {margin} in a \
                             proportion, which can never be breached; use a margin below 1"
                        ));
                    }
                }
                Tolerance::Relative(share) => {
                    if !(share.is_finite() && share >= 0.0) {
                        return invalid(format!(
                            "guardrail '{metric}' has relative tolerance {share}; use a finite \
                             share of zero or more"
                        ));
                    }
                    if guardrail.metric.direction == MetricDirection::HigherIsBetter && share >= 1.0
                    {
                        return invalid(format!(
                            "guardrail '{metric}' tolerates a relative drop of {share} in a metric \
                             where higher is better, which can never be breached; use a share \
                             below 1"
                        ));
                    }
                }
            }
        }
        if self.min_units_per_arm < 2 {
            return invalid(format!(
                "experiment '{}' has min_units_per_arm = {}; at least 2 units per arm are needed \
                 to estimate a variance",
                self.id, self.min_units_per_arm
            ));
        }
        if self.planned_units_per_arm < self.min_units_per_arm {
            return invalid(format!(
                "experiment '{}' plans {} units per arm, fewer than its minimum of {}",
                self.id, self.planned_units_per_arm, self.min_units_per_arm
            ));
        }
        if let Some(max) = self.max_units_per_arm
            && max < self.min_units_per_arm
        {
            return invalid(format!(
                "experiment '{}' stops at {max} units per arm, before its minimum of {} lets any \
                 test decide",
                self.id, self.min_units_per_arm
            ));
        }
        match self.method {
            AnalysisMethod::Sequential => {}
            AnalysisMethod::Bayesian(bayes) => {
                let prior = bayes.prior;
                if !(prior.alpha.is_finite()
                    && prior.alpha > 0.0
                    && prior.beta.is_finite()
                    && prior.beta > 0.0)
                {
                    return invalid(format!(
                        "experiment '{}' has prior Beta({}, {}); both parameters must be finite \
                         and positive",
                        self.id, prior.alpha, prior.beta
                    ));
                }
                if !(bayes.decision_probability > 0.5 && bayes.decision_probability < 1.0) {
                    return invalid(format!(
                        "experiment '{}' has decision_probability = {}; use a value in (0.5, 1)",
                        self.id, bayes.decision_probability
                    ));
                }
                if !(bayes.credible_level > 0.0 && bayes.credible_level < 1.0) {
                    return invalid(format!(
                        "experiment '{}' has credible_level = {}; use a value in (0, 1)",
                        self.id, bayes.credible_level
                    ));
                }
                if let Some(loss) = bayes.max_expected_loss
                    && !(loss.is_finite() && loss >= 0.0)
                {
                    return invalid(format!(
                        "experiment '{}' has max_expected_loss = {loss}; use a finite loss of \
                         zero or more",
                        self.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// Returns the per-guardrail error budget: `guardrail_alpha` split equally.
    pub(crate) fn alpha_per_guardrail(&self) -> f64 {
        self.budget.guardrail_alpha / self.guardrails.len().max(1) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ExperimentConfig {
        ExperimentConfig::new(
            "headline",
            MetricSpec::proportion("signups", MetricDirection::HigherIsBetter),
        )
    }

    fn message(config: &ExperimentConfig) -> String {
        match config.validate() {
            Err(EvalError::ConfigError(message)) => message,
            other => panic!("expected a configuration error, got {other:?}"),
        }
    }

    #[test]
    fn defaults_are_valid() {
        assert!(base().validate().is_ok());
        let bayes = base().with_method(AnalysisMethod::Bayesian(BayesianConfig::default()));
        assert!(bayes.validate().is_ok());
    }

    #[test]
    fn repeated_metric_ids_are_refused() {
        let config = base().with_guardrail(GuardrailSpec::new(
            MetricSpec::proportion("signups", MetricDirection::LowerIsBetter),
            Tolerance::Absolute(0.0),
        ));
        assert!(message(&config).contains("names metric 'signups' twice"));
    }

    #[test]
    fn budgets_outside_the_open_interval_are_refused() {
        for alpha in [0.0, -0.1, 0.6, f64::NAN] {
            let config = base().with_budget(ErrorBudget::new(alpha, 0.1));
            assert!(message(&config).contains("alpha"), "alpha {alpha}");
            let config = base().with_budget(ErrorBudget::new(0.05, alpha));
            assert!(message(&config).contains("guardrail_alpha"), "guardrail alpha {alpha}");
        }
    }

    #[test]
    fn unbreachable_tolerances_are_refused() {
        let drop = base().with_guardrail(GuardrailSpec::new(
            MetricSpec::mean("revenue", MetricDirection::HigherIsBetter),
            Tolerance::Relative(1.0),
        ));
        assert!(message(&drop).contains("can never be breached"));
        let absolute = base().with_guardrail(GuardrailSpec::new(
            MetricSpec::proportion("refunds", MetricDirection::LowerIsBetter),
            Tolerance::Absolute(1.0),
        ));
        assert!(message(&absolute).contains("can never be breached"));
        let negative = base().with_guardrail(GuardrailSpec::new(
            MetricSpec::proportion("refunds", MetricDirection::LowerIsBetter),
            Tolerance::Relative(-0.1),
        ));
        assert!(message(&negative).contains("relative tolerance"));
    }

    #[test]
    fn sizes_out_of_order_are_refused() {
        assert!(message(&base().with_min_units_per_arm(1)).contains("at least 2"));
        assert!(message(&base().with_planned_units_per_arm(50)).contains("plans 50"));
        assert!(message(&base().with_max_units_per_arm(50)).contains("stops at 50"));
    }

    #[test]
    fn bayesian_settings_out_of_range_are_refused() {
        let with = |bayes: BayesianConfig| base().with_method(AnalysisMethod::Bayesian(bayes));
        let prior = BayesianConfig::default().with_prior(BetaPrior::new(0.0, 1.0));
        assert!(message(&with(prior)).contains("prior"));
        let probability = BayesianConfig::default().with_decision_probability(0.5);
        assert!(message(&with(probability)).contains("decision_probability"));
        let level = BayesianConfig::default().with_credible_level(1.0);
        assert!(message(&with(level)).contains("credible_level"));
        let loss = BayesianConfig::default().with_max_expected_loss(-1.0);
        assert!(message(&with(loss)).contains("max_expected_loss"));
    }
}
