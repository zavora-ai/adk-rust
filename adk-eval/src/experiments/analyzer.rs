//! `ExperimentAnalyzer`: the sequential analysis of one experiment, look by look
//! (spec 3.5 §7.12, A-I15, §13 Q8).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::config::{
    AnalysisMethod, BayesianConfig, ExperimentConfig, GuardrailSpec, MetricDirection, Tolerance,
};
use super::normal;
use super::report::{
    Decision, EffectEstimate, Evidence, ExperimentReport, GuardrailAnalysis, Interval,
    PrimaryAnalysis, StopReason,
};
use super::sample::{ArmSamples, Look, Sample};
use super::sequential::SequentialBoundary;
use crate::error::{EvalError, Result};

/// The persistent state of an [`ExperimentAnalyzer`] between looks (spec 3.5 §7.12).
///
/// The always-valid p-values are running minima over every look, so a process that analyses an
/// experiment once a day keeps this state and resumes it with
/// [`ExperimentAnalyzer::resume`]. It serialises to camelCase JSON.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentState {
    experiment_id: String,
    looks: u32,
    last: Option<Look>,
    primary_p_value: f64,
    guardrail_p_values: BTreeMap<String, f64>,
    final_report: Option<ExperimentReport>,
}

impl ExperimentState {
    /// Returns the experiment's id.
    pub fn experiment_id(&self) -> &str {
        &self.experiment_id
    }

    /// Returns the number of looks analysed.
    pub fn looks(&self) -> u32 {
        self.looks
    }

    /// Returns the report of the look that stopped the experiment, if it has stopped.
    pub fn final_report(&self) -> Option<&ExperimentReport> {
        self.final_report.as_ref()
    }
}

/// Which way the primary metric's evidence points at a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Undecided,
    VariantBetter,
    VariantWorse,
}

/// The sequential analysis of a web experiment with one control and one variant
/// (spec 3.5 §7.12, A-I15, §13 Q8).
///
/// Each call to [`observe`](Self::observe) analyses one look at the cumulative data and returns
/// an [`ExperimentReport`]. Looks may come as often as the caller likes — daily is typical —
/// without spending more than the [`ErrorBudget`](super::ErrorBudget). Guardrails are checked
/// first at every look; a breach stops the experiment and keeps the control whatever the
/// primary metric shows. A final decision is kept: later looks return the same report.
///
/// See the [module documentation](super) for the method and its assumptions.
///
/// # Example
///
/// ```
/// use adk_eval::experiments::{
///     Decision, ExperimentAnalyzer, ExperimentConfig, GuardrailSpec, Look, MetricDirection,
///     MetricSpec, Sample, StopReason, Tolerance,
/// };
///
/// let config = ExperimentConfig::new(
///     "headline",
///     MetricSpec::proportion("signups", MetricDirection::HigherIsBetter),
/// )
/// .with_guardrail(GuardrailSpec::new(
///     MetricSpec::proportion("unsubscribes", MetricDirection::LowerIsBetter),
///     Tolerance::Relative(0.1),
/// ));
/// let mut analyzer = ExperimentAnalyzer::new(config).unwrap();
///
/// // Each day's look carries the totals so far: B lifts sign-ups from 4% to 5% but doubles
/// // unsubscribes from 0.5% to 1%.
/// let mut decision = Decision::Continue;
/// for day in 1..=28 {
///     let visitors = 600 * day;
///     let look = Look::new()
///         .with_metric(
///             "signups",
///             Sample::proportion(visitors, 24 * day),
///             Sample::proportion(visitors, 30 * day),
///         )
///         .with_metric(
///             "unsubscribes",
///             Sample::proportion(visitors, 3 * day),
///             Sample::proportion(visitors, 6 * day),
///         );
///     let report = analyzer.observe(&look).unwrap();
///     decision = report.decision;
///     if decision.is_final() {
///         break;
///     }
/// }
/// assert_eq!(
///     decision,
///     Decision::KeepControl {
///         reason: StopReason::GuardrailBreach { guardrails: vec!["unsubscribes".into()] }
///     }
/// );
/// ```
#[derive(Debug, Clone)]
pub struct ExperimentAnalyzer {
    config: ExperimentConfig,
    primary_boundary: SequentialBoundary,
    guardrail_boundary: SequentialBoundary,
    state: ExperimentState,
}

impl ExperimentAnalyzer {
    /// Creates an analyzer for a new experiment.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::ConfigError`] when the configuration is invalid; see
    /// [`ExperimentConfig::validate`].
    pub fn new(config: ExperimentConfig) -> Result<Self> {
        config.validate()?;
        let planned = config.planned_units_per_arm as f64 / 2.0;
        let primary_boundary = SequentialBoundary::new(config.budget.alpha, planned)?;
        let guardrail_boundary = SequentialBoundary::new(config.alpha_per_guardrail(), planned)?;
        let state = ExperimentState {
            experiment_id: config.id.clone(),
            looks: 0,
            last: None,
            primary_p_value: 1.0,
            guardrail_p_values: config
                .guardrails
                .iter()
                .map(|guardrail| (guardrail.metric.id.clone(), 1.0))
                .collect(),
            final_report: None,
        };
        Ok(Self { config, primary_boundary, guardrail_boundary, state })
    }

    /// Resumes an experiment from its saved state.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::ConfigError`] when the configuration is invalid or the state
    /// belongs to another experiment or another set of guardrails, and
    /// [`EvalError::StatisticsError`] when a saved p-value is outside `[0, 1]`.
    pub fn resume(config: ExperimentConfig, state: ExperimentState) -> Result<Self> {
        let mut analyzer = Self::new(config)?;
        if state.experiment_id != analyzer.config.id {
            return Err(EvalError::ConfigError(format!(
                "state of experiment '{}' cannot resume experiment '{}'",
                state.experiment_id, analyzer.config.id
            )));
        }
        let saved: BTreeSet<&String> = state.guardrail_p_values.keys().collect();
        let configured: BTreeSet<&String> =
            analyzer.config.guardrails.iter().map(|guardrail| &guardrail.metric.id).collect();
        if saved != configured {
            return Err(EvalError::ConfigError(format!(
                "experiment '{}' was saved with guardrails {saved:?} but is configured with \
                 {configured:?}; guardrails are fixed for an experiment's life",
                analyzer.config.id
            )));
        }
        let p_values =
            std::iter::once(&state.primary_p_value).chain(state.guardrail_p_values.values());
        for p_value in p_values {
            if !(0.0..=1.0).contains(p_value) {
                return Err(EvalError::StatisticsError(format!(
                    "saved state of experiment '{}' has p-value {p_value}; p-values lie in [0, 1]",
                    analyzer.config.id
                )));
            }
        }
        analyzer.state = state;
        Ok(analyzer)
    }

    /// Returns the configuration.
    pub fn config(&self) -> &ExperimentConfig {
        &self.config
    }

    /// Returns the state to save between looks.
    pub fn state(&self) -> &ExperimentState {
        &self.state
    }

    /// Returns whether the experiment has reached a final decision.
    pub fn is_stopped(&self) -> bool {
        self.state.final_report.is_some()
    }

    /// Analyses one look at the cumulative data and returns the decision with its evidence.
    ///
    /// Once a look reaches a final decision, every later look returns that look's report
    /// unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::StatisticsError`] when the look misses the primary metric or a
    /// guardrail, carries a sample of the wrong kind or an invalid sample, or has fewer units
    /// or events than the previous look (looks are cumulative). A refused look leaves the
    /// state unchanged.
    pub fn observe(&mut self, look: &Look) -> Result<ExperimentReport> {
        if let Some(report) = &self.state.final_report {
            return Ok(report.clone());
        }
        let look_number = self.state.looks + 1;
        self.check_look(look, look_number)?;

        let mut guardrail_p_values = BTreeMap::new();
        let mut guardrails = Vec::with_capacity(self.config.guardrails.len());
        for spec in &self.config.guardrails {
            let arms = Self::arms(look, &spec.metric.id);
            let previous =
                self.state.guardrail_p_values.get(&spec.metric.id).copied().unwrap_or(1.0);
            let analysis = self.guardrail(spec, arms, previous);
            guardrail_p_values.insert(spec.metric.id.clone(), analysis.p_value);
            guardrails.push(analysis);
        }

        let arms = Self::arms(look, &self.config.primary.id);
        let (primary, primary_p_value, verdict) = match self.config.method {
            AnalysisMethod::Sequential => self.sequential_primary(arms),
            AnalysisMethod::Bayesian(bayes) => self.bayesian_primary(arms, bayes),
        };

        let breached: Vec<String> = guardrails
            .iter()
            .filter(|guardrail| guardrail.breached)
            .map(|guardrail| guardrail.metric.clone())
            .collect();
        // Adoption also needs every guardrail measured and, on its estimate, within tolerance:
        // a harm not yet proven still holds the variant back until a breach or more data.
        let guardrails_clear =
            guardrails.iter().all(|guardrail| guardrail.ready && guardrail.harm <= 0.0);
        let at_limit = self
            .config
            .max_units_per_arm
            .is_some_and(|max| arms.control.units() >= max && arms.variant.units() >= max);
        // Guardrails come first: a breach keeps the control whatever the primary shows (A-I15).
        let decision = if !breached.is_empty() {
            Decision::KeepControl { reason: StopReason::GuardrailBreach { guardrails: breached } }
        } else {
            match verdict {
                Verdict::VariantBetter if guardrails_clear => Decision::AdoptVariant,
                Verdict::VariantWorse => Decision::KeepControl { reason: StopReason::VariantWorse },
                Verdict::VariantBetter | Verdict::Undecided if at_limit => {
                    Decision::KeepControl { reason: StopReason::SampleLimitReached }
                }
                Verdict::VariantBetter | Verdict::Undecided => Decision::Continue,
            }
        };

        let report = ExperimentReport {
            experiment_id: self.config.id.clone(),
            look: look_number,
            primary,
            guardrails,
            decision,
        };
        self.state.looks = look_number;
        self.state.last = Some(look.clone());
        self.state.primary_p_value = primary_p_value;
        self.state.guardrail_p_values = guardrail_p_values;
        if report.decision.is_final() {
            info!(
                experiment.id = %self.config.id,
                experiment.look = look_number,
                experiment.decision = %report.decision,
                "experiment stopped"
            );
            self.state.final_report = Some(report.clone());
        } else {
            debug!(experiment.id = %self.config.id, experiment.look = look_number, "experiment continues");
        }
        Ok(report)
    }

    /// Returns the samples of a metric `check_look` has confirmed are present.
    fn arms<'a>(look: &'a Look, metric: &str) -> &'a ArmSamples {
        look.metrics.get(metric).expect("check_look confirms every configured metric is present")
    }

    /// Checks that the look carries every configured metric, valid and cumulative.
    fn check_look(&self, look: &Look, look_number: u32) -> Result<()> {
        let specs = std::iter::once(&self.config.primary)
            .chain(self.config.guardrails.iter().map(|guardrail| &guardrail.metric));
        for spec in specs {
            let id = &spec.id;
            let Some(arms) = look.metric(id) else {
                return Err(EvalError::StatisticsError(format!(
                    "look {look_number} of experiment '{}' has no samples for metric '{id}'; \
                     every look carries the primary metric and every guardrail",
                    self.config.id
                )));
            };
            for (arm, sample) in [("control", &arms.control), ("variant", &arms.variant)] {
                if sample.kind() != spec.kind {
                    return Err(EvalError::StatisticsError(format!(
                        "look {look_number} carries a {:?} sample for the {arm} of metric \
                         '{id}', which is configured as {:?}",
                        sample.kind(),
                        spec.kind
                    )));
                }
                sample.check(&format!("look {look_number}, metric '{id}', {arm}"))?;
            }
            let Some(previous) = self.state.last.as_ref().and_then(|last| last.metric(id)) else {
                continue;
            };
            for (arm, now, before) in [
                ("control", &arms.control, &previous.control),
                ("variant", &arms.variant, &previous.variant),
            ] {
                let fewer_events = matches!(
                    (now, before),
                    (Sample::Proportion { events: now, .. }, Sample::Proportion { events: before, .. })
                        if now < before
                );
                if now.units() < before.units() || fewer_events {
                    return Err(EvalError::StatisticsError(format!(
                        "look {look_number} has fewer units or events for the {arm} of metric \
                         '{id}' than look {}; looks carry cumulative totals since the start",
                        self.state.looks
                    )));
                }
            }
        }
        Ok(())
    }

    fn ready(&self, arms: &ArmSamples) -> bool {
        arms.control.units() >= self.config.min_units_per_arm
            && arms.variant.units() >= self.config.min_units_per_arm
    }

    /// Analyses one guardrail with the one-sided sequential test of "harm within tolerance".
    fn guardrail(
        &self,
        spec: &GuardrailSpec,
        arms: &ArmSamples,
        previous: f64,
    ) -> GuardrailAnalysis {
        let (control, variant) = (arms.control.estimate(), arms.variant.estimate());
        let (control_var, variant_var) =
            (arms.control.variance_of_estimate(), arms.variant.variance_of_estimate());
        // harm = variant − c·control − m (lower is better) or c·control − variant − m (higher
        // is better), so the guardrail holds while harm ≤ 0.
        let (scale, margin) = match (spec.metric.direction, spec.tolerance) {
            (_, Tolerance::Absolute(margin)) => (1.0, margin),
            (MetricDirection::LowerIsBetter, Tolerance::Relative(share)) => (1.0 + share, 0.0),
            (MetricDirection::HigherIsBetter, Tolerance::Relative(share)) => (1.0 - share, 0.0),
        };
        let harm = match spec.metric.direction {
            MetricDirection::LowerIsBetter => variant - scale * control - margin,
            MetricDirection::HigherIsBetter => scale * control - variant - margin,
        };
        let se = (variant_var + scale * scale * control_var).sqrt();
        let units = arms.effective_units();
        let ready = self.ready(arms);
        let z = if se > 0.0 { harm / se } else { 0.0 };
        let look_p = if ready && z > 0.0 { self.guardrail_boundary.p_value(z, units) } else { 1.0 };
        let p_value = previous.min(look_p);
        let critical = ready.then(|| self.guardrail_boundary.critical_z(units));
        GuardrailAnalysis {
            metric: spec.metric.id.clone(),
            kind: spec.metric.kind,
            direction: spec.metric.direction,
            tolerance: spec.tolerance,
            control_units: arms.control.units(),
            variant_units: arms.variant.units(),
            ready,
            effect: effect(
                (control, control_var),
                (variant, variant_var),
                critical,
                1.0 - self.guardrail_boundary.alpha(),
            ),
            harm,
            harm_lower_bound: critical
                .filter(|critical| critical.is_finite() && se > 0.0)
                .map(|critical| harm - critical * se),
            p_value,
            alpha: self.guardrail_boundary.alpha(),
            breached: p_value <= self.guardrail_boundary.alpha(),
        }
    }

    /// Analyses the primary metric with the two-sided sequential test of "no difference".
    fn sequential_primary(&self, arms: &ArmSamples) -> (PrimaryAnalysis, f64, Verdict) {
        let (control, variant) = (arms.control.estimate(), arms.variant.estimate());
        let (control_var, variant_var) =
            (arms.control.variance_of_estimate(), arms.variant.variance_of_estimate());
        let difference = variant - control;
        let se = (control_var + variant_var).sqrt();
        let units = arms.effective_units();
        let ready = self.ready(arms);
        let z = if se > 0.0 { difference / se } else { 0.0 };
        let look_p = if ready { self.primary_boundary.p_value(z, units) } else { 1.0 };
        let p_value = self.state.primary_p_value.min(look_p);
        let critical = ready.then(|| self.primary_boundary.critical_z(units));
        let improvement = difference * self.config.primary.direction.sign();
        let verdict = if p_value > self.primary_boundary.alpha() || improvement == 0.0 {
            Verdict::Undecided
        } else if improvement > 0.0 {
            Verdict::VariantBetter
        } else {
            Verdict::VariantWorse
        };
        let alpha = self.primary_boundary.alpha();
        let analysis = self.primary_analysis(
            arms,
            ready,
            effect((control, control_var), (variant, variant_var), critical, 1.0 - alpha),
            Evidence::Sequential { p_value, z, critical_z: critical, alpha },
        );
        (analysis, p_value, verdict)
    }

    /// Analyses the primary metric by its posterior (spec 3.5 §13 Q8).
    fn bayesian_primary(
        &self,
        arms: &ArmSamples,
        bayes: BayesianConfig,
    ) -> (PrimaryAnalysis, f64, Verdict) {
        let posterior = |sample: &Sample| match *sample {
            Sample::Proportion { units, events } => {
                let successes = bayes.prior.alpha + events as f64;
                let total = bayes.prior.alpha + bayes.prior.beta + units as f64;
                let mean = successes / total;
                (mean, mean * (1.0 - mean) / (total + 1.0))
            }
            Sample::Mean { units: 0, .. } => (0.0, 0.0),
            Sample::Mean { units, mean, variance } => (mean, variance / units as f64),
        };
        let (control, control_var) = posterior(&arms.control);
        let (variant, variant_var) = posterior(&arms.variant);
        let ready = self.ready(arms);
        // The posterior of the improvement (variant over control, in the better direction) is
        // approximately normal with mean `gain` and standard deviation `spread`.
        let gain = (variant - control) * self.config.primary.direction.sign();
        let spread = (control_var + variant_var).sqrt();
        let (probability, loss_adopt, loss_keep) = if spread > 0.0 {
            let ratio = gain / spread;
            let density = spread * normal::pdf(ratio);
            (
                normal::cdf(ratio),
                density - gain * normal::cdf(-ratio),
                density + gain * normal::cdf(ratio),
            )
        } else {
            let probability = match gain.partial_cmp(&0.0) {
                Some(std::cmp::Ordering::Greater) => 1.0,
                Some(std::cmp::Ordering::Less) => 0.0,
                Some(std::cmp::Ordering::Equal) | None => 0.5,
            };
            (probability, (-gain).max(0.0), gain.max(0.0))
        };
        let within_loss = |loss: f64| bayes.max_expected_loss.is_none_or(|max| loss <= max);
        let verdict = if !ready {
            Verdict::Undecided
        } else if probability >= bayes.decision_probability && within_loss(loss_adopt) {
            Verdict::VariantBetter
        } else if probability <= 1.0 - bayes.decision_probability && within_loss(loss_keep) {
            Verdict::VariantWorse
        } else {
            Verdict::Undecided
        };
        let critical = ready.then(|| normal::quantile(0.5 + bayes.credible_level / 2.0));
        let analysis = self.primary_analysis(
            arms,
            ready,
            effect((control, control_var), (variant, variant_var), critical, bayes.credible_level),
            Evidence::Bayesian {
                probability_variant_better: probability,
                expected_loss_adopt: loss_adopt,
                expected_loss_keep: loss_keep,
                decision_probability: bayes.decision_probability,
            },
        );
        (analysis, self.state.primary_p_value, verdict)
    }

    fn primary_analysis(
        &self,
        arms: &ArmSamples,
        ready: bool,
        effect: EffectEstimate,
        evidence: Evidence,
    ) -> PrimaryAnalysis {
        PrimaryAnalysis {
            metric: self.config.primary.id.clone(),
            kind: self.config.primary.kind,
            direction: self.config.primary.direction,
            control_units: arms.control.units(),
            variant_units: arms.variant.units(),
            ready,
            effect,
            evidence,
        }
    }
}

/// Builds the effect estimate from each arm's estimate and its variance; `critical` scales the
/// standard errors into interval half-widths. An estimate with no variance gets no interval:
/// a normal interval of width zero would claim certainty the data cannot give.
fn effect(
    (control, control_var): (f64, f64),
    (variant, variant_var): (f64, f64),
    critical: Option<f64>,
    level: f64,
) -> EffectEstimate {
    let difference = variant - control;
    let critical = critical.filter(|critical| critical.is_finite());
    let interval = |center: f64, se: f64| {
        critical.filter(|_| se > 0.0).map(|critical| Interval::around(center, critical * se, level))
    };
    let difference_interval = interval(difference, (control_var + variant_var).sqrt());
    let relative_lift = (control != 0.0).then(|| difference / control);
    // Delta method for variant/control − 1.
    let relative_interval = relative_lift.and_then(|lift| {
        let control_sq = control * control;
        let se = (variant_var / control_sq
            + variant * variant * control_var / (control_sq * control_sq))
            .sqrt();
        interval(lift, se)
    });
    EffectEstimate {
        control,
        variant,
        difference,
        difference_interval,
        relative_lift,
        relative_interval,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::experiments::{ErrorBudget, MetricSpec};

    fn signups() -> MetricSpec {
        MetricSpec::proportion("signups", MetricDirection::HigherIsBetter)
    }

    fn proportions(control: (u64, u64), variant: (u64, u64)) -> Look {
        Look::new().with_metric(
            "signups",
            Sample::proportion(control.0, control.1),
            Sample::proportion(variant.0, variant.1),
        )
    }

    #[test]
    fn a_single_look_matches_the_two_proportion_z_statistic() {
        // 200/1000 against 250/1000: unpooled se 0.0186414, z 2.68221 (computed by hand).
        let mut analyzer = ExperimentAnalyzer::new(ExperimentConfig::new("t", signups())).unwrap();
        let report = analyzer.observe(&proportions((1000, 200), (1000, 250))).unwrap();
        let Evidence::Sequential { z, critical_z, p_value, alpha } = report.primary.evidence else {
            panic!("sequential evidence")
        };
        assert!((z - 2.6822089039290997).abs() < 1e-12);
        assert_eq!(alpha, 0.05);
        let boundary = SequentialBoundary::new(0.05, 5_000.0).unwrap();
        assert_eq!(critical_z, Some(boundary.critical_z(500.0)));
        assert_eq!(p_value, boundary.p_value(z, 500.0));
        let interval = report.primary.effect.difference_interval.unwrap();
        let half_width = boundary.critical_z(500.0) * 0.01864135188230725;
        assert!((interval.lower - (0.05 - half_width)).abs() < 1e-12);
        assert!((interval.upper - (0.05 + half_width)).abs() < 1e-12);
        assert_eq!(interval.level, 0.95);
        assert!((report.primary.effect.relative_lift.unwrap() - 0.25).abs() < 1e-12);
    }

    #[test]
    fn nothing_decides_before_the_minimum_units() {
        let config = ExperimentConfig::new("t", signups()).with_min_units_per_arm(500);
        let mut analyzer = ExperimentAnalyzer::new(config).unwrap();
        let report = analyzer.observe(&proportions((400, 4), (400, 200))).unwrap();
        assert_eq!(report.decision, Decision::Continue);
        assert!(!report.primary.ready);
        assert_eq!(report.primary.effect.difference_interval, None);
        let Evidence::Sequential { p_value, critical_z, .. } = report.primary.evidence else {
            panic!("sequential evidence")
        };
        assert_eq!((p_value, critical_z), (1.0, None));
    }

    #[test]
    fn a_worse_variant_keeps_the_control() {
        let mut analyzer = ExperimentAnalyzer::new(ExperimentConfig::new("t", signups())).unwrap();
        let report = analyzer.observe(&proportions((5000, 1000), (5000, 700))).unwrap();
        assert_eq!(report.decision, Decision::KeepControl { reason: StopReason::VariantWorse });
    }

    #[test]
    fn a_better_variant_is_adopted_and_the_decision_is_final() {
        let mut analyzer = ExperimentAnalyzer::new(ExperimentConfig::new("t", signups())).unwrap();
        let report = analyzer.observe(&proportions((5000, 700), (5000, 1000))).unwrap();
        assert_eq!(report.decision, Decision::AdoptVariant);
        assert!(analyzer.is_stopped());
        // A later look, even one that reverses the effect, returns the final report.
        let later = analyzer.observe(&proportions((9000, 1800), (9000, 1000))).unwrap();
        assert_eq!(later, report);
    }

    #[test]
    fn the_sample_limit_keeps_the_control() {
        let config = ExperimentConfig::new("t", signups()).with_max_units_per_arm(2000);
        let mut analyzer = ExperimentAnalyzer::new(config).unwrap();
        let first = analyzer.observe(&proportions((1000, 100), (1000, 101))).unwrap();
        assert_eq!(first.decision, Decision::Continue);
        let second = analyzer.observe(&proportions((2000, 200), (2000, 203))).unwrap();
        assert_eq!(
            second.decision,
            Decision::KeepControl { reason: StopReason::SampleLimitReached }
        );
    }

    #[test]
    fn looks_must_be_cumulative_and_complete() {
        let config = ExperimentConfig::new("t", signups()).with_guardrail(GuardrailSpec::new(
            MetricSpec::mean("revenue", MetricDirection::HigherIsBetter),
            Tolerance::Relative(0.05),
        ));
        let mut analyzer = ExperimentAnalyzer::new(config).unwrap();
        let missing = analyzer.observe(&proportions((100, 1), (100, 2)));
        assert!(matches!(missing, Err(EvalError::StatisticsError(m)) if m.contains("'revenue'")));
        let look = |units: u64, events: u64| {
            proportions((units, events), (units, events)).with_metric(
                "revenue",
                Sample::mean(units, 2.0, 1.0),
                Sample::mean(units, 2.0, 1.0),
            )
        };
        analyzer.observe(&look(200, 20)).unwrap();
        let shrunk = analyzer.observe(&look(150, 20));
        assert!(matches!(shrunk, Err(EvalError::StatisticsError(m)) if m.contains("cumulative")));
        let fewer_events = analyzer.observe(&look(300, 10));
        assert!(
            matches!(fewer_events, Err(EvalError::StatisticsError(m)) if m.contains("cumulative"))
        );
        let wrong_kind = look(300, 30).with_metric(
            "revenue",
            Sample::proportion(300, 1),
            Sample::proportion(300, 1),
        );
        assert!(analyzer.observe(&wrong_kind).is_err());
        // Refused looks leave the state unchanged.
        assert_eq!(analyzer.state().looks(), 1);
        assert_eq!(analyzer.observe(&look(300, 30)).unwrap().look, 2);
    }

    #[test]
    fn guardrails_override_a_winning_primary_metric() {
        let config = ExperimentConfig::new("t", signups()).with_guardrail(GuardrailSpec::new(
            MetricSpec::proportion("refunds", MetricDirection::LowerIsBetter),
            Tolerance::Absolute(0.0),
        ));
        let mut analyzer = ExperimentAnalyzer::new(config).unwrap();
        let look = proportions((5000, 700), (5000, 1000)).with_metric(
            "refunds",
            Sample::proportion(5000, 50),
            Sample::proportion(5000, 150),
        );
        let report = analyzer.observe(&look).unwrap();
        let Evidence::Sequential { p_value, alpha, .. } = report.primary.evidence else {
            panic!("sequential evidence")
        };
        assert!(p_value <= alpha, "the primary metric alone would adopt the variant");
        assert_eq!(
            report.decision,
            Decision::KeepControl {
                reason: StopReason::GuardrailBreach { guardrails: vec!["refunds".into()] }
            }
        );
    }

    #[test]
    fn an_unproven_harm_holds_adoption_back() {
        let config = ExperimentConfig::new("t", signups()).with_guardrail(GuardrailSpec::new(
            MetricSpec::proportion("refunds", MetricDirection::LowerIsBetter),
            Tolerance::Relative(0.1),
        ));
        let mut analyzer = ExperimentAnalyzer::new(config).unwrap();
        // Refunds look 20% higher in the variant, beyond the 10% tolerance but far from proven.
        let first = analyzer
            .observe(&proportions((5000, 700), (5000, 1000)).with_metric(
                "refunds",
                Sample::proportion(5000, 50),
                Sample::proportion(5000, 60),
            ))
            .unwrap();
        assert!(!first.guardrails[0].breached && first.guardrails[0].harm > 0.0);
        assert_eq!(first.decision, Decision::Continue);
        // With more data the refunds estimate falls back within tolerance.
        let second = analyzer
            .observe(&proportions((10_000, 1400), (10_000, 2000)).with_metric(
                "refunds",
                Sample::proportion(10_000, 100),
                Sample::proportion(10_000, 105),
            ))
            .unwrap();
        assert!(second.guardrails[0].harm <= 0.0);
        assert_eq!(second.decision, Decision::AdoptVariant);
    }

    #[test]
    fn adoption_waits_for_every_guardrail_to_reach_the_minimum() {
        let config = ExperimentConfig::new("t", signups()).with_guardrail(GuardrailSpec::new(
            MetricSpec::proportion("unsubscribes", MetricDirection::LowerIsBetter),
            Tolerance::Relative(0.1),
        ));
        let mut analyzer = ExperimentAnalyzer::new(config).unwrap();
        // The guardrail counts unsubscribes among sign-ups, which lag the visitors.
        let look = proportions((5000, 700), (5000, 1000)).with_metric(
            "unsubscribes",
            Sample::proportion(70, 1),
            Sample::proportion(90, 1),
        );
        let report = analyzer.observe(&look).unwrap();
        assert!(!report.guardrails[0].ready);
        assert_eq!(report.decision, Decision::Continue);
    }

    #[test]
    fn guardrail_budget_is_split_across_guardrails() {
        let guardrail = |id: &str| {
            GuardrailSpec::new(
                MetricSpec::proportion(id, MetricDirection::LowerIsBetter),
                Tolerance::Absolute(0.0),
            )
        };
        let config = ExperimentConfig::new("t", signups())
            .with_budget(ErrorBudget::new(0.05, 0.10))
            .with_guardrail(guardrail("refunds"))
            .with_guardrail(guardrail("complaints"));
        let mut analyzer = ExperimentAnalyzer::new(config).unwrap();
        let look = proportions((1000, 100), (1000, 100))
            .with_metric("refunds", Sample::proportion(1000, 10), Sample::proportion(1000, 10))
            .with_metric("complaints", Sample::proportion(1000, 5), Sample::proportion(1000, 5));
        let report = analyzer.observe(&look).unwrap();
        assert!(report.guardrails.iter().all(|guardrail| guardrail.alpha == 0.05));
    }

    #[test]
    fn resume_continues_the_running_p_values() {
        let config = ExperimentConfig::new("t", signups());
        let mut analyzer = ExperimentAnalyzer::new(config.clone()).unwrap();
        analyzer.observe(&proportions((1000, 100), (1000, 130))).unwrap();
        let saved = serde_json::to_string(analyzer.state()).unwrap();
        let state: ExperimentState = serde_json::from_str(&saved).unwrap();
        let mut resumed = ExperimentAnalyzer::resume(config.clone(), state).unwrap();
        let next = proportions((2000, 200), (2000, 230));
        let (resumed_report, report) =
            (resumed.observe(&next).unwrap(), analyzer.observe(&next).unwrap());
        assert_eq!(resumed_report.decision, report.decision);
        let p_value = |report: &ExperimentReport| match report.primary.evidence {
            Evidence::Sequential { p_value, .. } => p_value,
            Evidence::Bayesian { .. } => panic!("sequential evidence"),
        };
        // JSON keeps floats to within the last digit.
        assert!((p_value(&resumed_report) - p_value(&report)).abs() < 1e-12);

        let other = ExperimentConfig::new("other", signups());
        assert!(ExperimentAnalyzer::resume(other, resumed.state().clone()).is_err());
        let more_guardrails = config.with_guardrail(GuardrailSpec::new(
            MetricSpec::proportion("refunds", MetricDirection::LowerIsBetter),
            Tolerance::Absolute(0.0),
        ));
        assert!(ExperimentAnalyzer::resume(more_guardrails, resumed.state().clone()).is_err());
    }
}
