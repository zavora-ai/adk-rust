//! The analysis of one look: effect estimates, evidence, guardrails and the decision
//! (spec 3.5 §7.12, A-I15).

use std::fmt;

use serde::{Deserialize, Serialize};

use super::config::{MetricDirection, MetricKind, Tolerance};

/// An interval estimate of an effect (spec 3.5 §7.12).
///
/// Sequential analyses report a confidence sequence, which covers the true effect at every look
/// at once with probability `level`; the Bayesian analysis reports a credible interval.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Interval {
    /// The lower end.
    pub lower: f64,
    /// The upper end.
    pub upper: f64,
    /// The coverage level, such as 0.95.
    pub level: f64,
}

impl Interval {
    pub(crate) fn around(center: f64, half_width: f64, level: f64) -> Self {
        Self { lower: center - half_width, upper: center + half_width, level }
    }

    /// Returns whether `value` lies in the interval, ends included.
    pub fn contains(&self, value: f64) -> bool {
        self.lower <= value && value <= self.upper
    }
}

/// The effect of the variant on one metric (spec 3.5 §7.12).
///
/// Differences are variant minus control in the metric's units: a rate for a proportion, a
/// mean for a mean. Intervals are `None` until both arms reach the minimum units.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct EffectEstimate {
    /// The control's estimate.
    pub control: f64,
    /// The variant's estimate.
    pub variant: f64,
    /// Variant minus control.
    pub difference: f64,
    /// The interval of the difference.
    pub difference_interval: Option<Interval>,
    /// The difference relative to the control; `None` when the control's estimate is zero.
    pub relative_lift: Option<f64>,
    /// The interval of the relative lift, by the delta method.
    pub relative_interval: Option<Interval>,
}

/// The evidence behind the primary metric's verdict (spec 3.5 §7.12, §13 Q8).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "method")]
#[non_exhaustive]
pub enum Evidence {
    /// The always-valid sequential test.
    #[serde(rename_all = "camelCase")]
    Sequential {
        /// The always-valid p-value: the running minimum over every look so far.
        p_value: f64,
        /// This look's z statistic.
        z: f64,
        /// The |z| this look needs; `None` until both arms reach the minimum units.
        critical_z: Option<f64>,
        /// The test's error budget.
        alpha: f64,
    },
    /// The Bayesian analysis.
    #[serde(rename_all = "camelCase")]
    Bayesian {
        /// The posterior probability that the variant is better than the control.
        probability_variant_better: f64,
        /// The expected loss, in the metric's units, of adopting the variant.
        expected_loss_adopt: f64,
        /// The expected loss, in the metric's units, of keeping the control.
        expected_loss_keep: f64,
        /// The probability needed to decide.
        decision_probability: f64,
    },
}

/// The analysis of the primary metric at one look (spec 3.5 §7.12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PrimaryAnalysis {
    /// The metric's id.
    pub metric: String,
    /// How the metric is measured.
    pub kind: MetricKind,
    /// Which way the metric improves.
    pub direction: MetricDirection,
    /// Units in the control.
    pub control_units: u64,
    /// Units in the variant.
    pub variant_units: u64,
    /// Whether both arms reached the minimum units, so the test may decide.
    pub ready: bool,
    /// The variant's effect.
    pub effect: EffectEstimate,
    /// The evidence for a decision.
    pub evidence: Evidence,
}

/// The analysis of one guardrail at one look (spec 3.5 §7.12, A-I15).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct GuardrailAnalysis {
    /// The metric's id.
    pub metric: String,
    /// How the metric is measured.
    pub kind: MetricKind,
    /// Which way the metric improves.
    pub direction: MetricDirection,
    /// How much worse than the control the variant may be.
    pub tolerance: Tolerance,
    /// Units in the control.
    pub control_units: u64,
    /// Units in the variant.
    pub variant_units: u64,
    /// Whether both arms reached the minimum units, so the guardrail may be breached.
    pub ready: bool,
    /// The variant's effect, with intervals at the guardrail's budget.
    pub effect: EffectEstimate,
    /// The estimated harm beyond the tolerance, in the metric's units; positive means worse
    /// than tolerated.
    pub harm: f64,
    /// The always-valid lower confidence bound of the harm; `None` until ready.
    pub harm_lower_bound: Option<f64>,
    /// The always-valid one-sided p-value of "harm within tolerance": the running minimum.
    pub p_value: f64,
    /// This guardrail's share of the guardrail error budget.
    pub alpha: f64,
    /// Whether the guardrail is breached.
    pub breached: bool,
}

/// What the experiment should do after a look (spec 3.5 §7.12, A-I15).
///
/// Every decision other than [`Decision::Continue`] is final: the analyzer returns the same
/// report for every later look.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "outcome")]
#[non_exhaustive]
pub enum Decision {
    /// Keep collecting data.
    Continue,
    /// The variant is better on the primary metric, no guardrail is breached, and every
    /// guardrail's estimated harm is within its tolerance.
    AdoptVariant,
    /// Stop and keep the control.
    KeepControl {
        /// Why the control is kept.
        reason: StopReason,
    },
}

impl Decision {
    /// Returns whether the experiment has stopped.
    pub fn is_final(&self) -> bool {
        !matches!(self, Self::Continue)
    }

    /// Returns whether the experiment stopped keeping the control.
    pub fn keeps_control(&self) -> bool {
        matches!(self, Self::KeepControl { .. })
    }
}

/// Why an experiment kept the control (spec 3.5 §7.12, A-I15).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[non_exhaustive]
pub enum StopReason {
    /// One or more guardrails were breached; this overrides the primary metric (A-I15).
    #[serde(rename_all = "camelCase")]
    GuardrailBreach {
        /// The breached guardrails' metric ids.
        guardrails: Vec<String>,
    },
    /// The variant is worse on the primary metric.
    VariantWorse,
    /// Both arms reached the maximum units without a decision.
    SampleLimitReached,
}

/// The analysis of one look at an experiment: the decision and its evidence (spec 3.5 §7.12).
///
/// Its [`Display`](fmt::Display) form is the evidence block the weekly review shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ExperimentReport {
    /// The experiment's id.
    pub experiment_id: String,
    /// The look this report analyses, counted from 1.
    pub look: u32,
    /// The primary metric.
    pub primary: PrimaryAnalysis,
    /// Every guardrail, in configuration order.
    pub guardrails: Vec<GuardrailAnalysis>,
    /// What the experiment should do.
    pub decision: Decision,
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Continue => f.write_str("continue"),
            Self::AdoptVariant => f.write_str("adopt the variant"),
            Self::KeepControl { reason } => write!(f, "keep the control — {reason}"),
        }
    }
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GuardrailBreach { guardrails } => {
                write!(f, "guardrail breached: {}", guardrails.join(", "))
            }
            Self::VariantWorse => f.write_str("the variant is worse on the primary metric"),
            Self::SampleLimitReached => f.write_str("sample limit reached without a decision"),
        }
    }
}

impl fmt::Display for MetricDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HigherIsBetter => f.write_str("higher is better"),
            Self::LowerIsBetter => f.write_str("lower is better"),
        }
    }
}

/// Formats a value in the metric's units: a rate as a percentage, a mean as a number.
fn value(kind: MetricKind, value: f64) -> String {
    match kind {
        MetricKind::Proportion => format!("{:.2}%", value * 100.0),
        MetricKind::Mean => format!("{value:.4}"),
    }
}

/// Formats a difference in the metric's units: percentage points for a rate.
fn difference(kind: MetricKind, value: f64) -> String {
    match kind {
        MetricKind::Proportion => format!("{:+.2} pp", value * 100.0),
        MetricKind::Mean => format!("{value:+.4}"),
    }
}

fn level(interval: &Interval) -> String {
    format!("{}%", (interval.level * 100.0 * 10.0).round() / 10.0)
}

fn write_effect(
    f: &mut fmt::Formatter<'_>,
    kind: MetricKind,
    effect: &EffectEstimate,
) -> fmt::Result {
    write!(f, "  difference {}", difference(kind, effect.difference))?;
    if let Some(interval) = &effect.difference_interval {
        write!(
            f,
            ", {} interval [{}, {}]",
            level(interval),
            difference(kind, interval.lower),
            difference(kind, interval.upper)
        )?;
    }
    if let Some(lift) = effect.relative_lift {
        write!(f, "; relative lift {:+.1}%", lift * 100.0)?;
        if let Some(interval) = &effect.relative_interval {
            write!(
                f,
                ", {} interval [{:+.1}%, {:+.1}%]",
                level(interval),
                interval.lower * 100.0,
                interval.upper * 100.0
            )?;
        }
    }
    writeln!(f)
}

impl fmt::Display for ExperimentReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Experiment '{}', look {}: {}", self.experiment_id, self.look, self.decision)?;
        let primary = &self.primary;
        writeln!(
            f,
            "Primary {} ({}): control {} of {}, variant {} of {}",
            primary.metric,
            primary.direction,
            value(primary.kind, primary.effect.control),
            primary.control_units,
            value(primary.kind, primary.effect.variant),
            primary.variant_units
        )?;
        write_effect(f, primary.kind, &primary.effect)?;
        match primary.evidence {
            Evidence::Sequential { p_value, z, critical_z, alpha } => {
                write!(f, "  always-valid p {p_value:.4} against alpha {alpha}; z {z:.2}")?;
                match critical_z {
                    Some(critical) => writeln!(f, " against {critical:.2}")?,
                    None => writeln!(f, ", below the minimum units")?,
                }
            }
            Evidence::Bayesian {
                probability_variant_better,
                expected_loss_adopt,
                expected_loss_keep,
                decision_probability,
            } => writeln!(
                f,
                "  P(variant better) {probability_variant_better:.4} against \
                 {decision_probability}; expected loss {} adopting, {} keeping",
                difference(primary.kind, expected_loss_adopt),
                difference(primary.kind, expected_loss_keep)
            )?,
        }
        for guardrail in &self.guardrails {
            let tolerance = match guardrail.tolerance {
                Tolerance::Absolute(margin) => {
                    format!("{} absolute", difference(guardrail.kind, margin))
                }
                Tolerance::Relative(share) => format!("{:.1}% relative", share * 100.0),
            };
            writeln!(
                f,
                "Guardrail {} ({}, tolerance {tolerance}): control {} of {}, variant {} of {}",
                guardrail.metric,
                guardrail.direction,
                value(guardrail.kind, guardrail.effect.control),
                guardrail.control_units,
                value(guardrail.kind, guardrail.effect.variant),
                guardrail.variant_units
            )?;
            write_effect(f, guardrail.kind, &guardrail.effect)?;
            write!(f, "  harm beyond tolerance {}", difference(guardrail.kind, guardrail.harm))?;
            if let Some(bound) = guardrail.harm_lower_bound {
                write!(f, ", lower bound {}", difference(guardrail.kind, bound))?;
            }
            writeln!(
                f,
                "; always-valid p {:.4} against alpha {:.4}: {}",
                guardrail.p_value,
                guardrail.alpha,
                if guardrail.breached { "breached" } else { "holding" }
            )?;
        }
        Ok(())
    }
}
