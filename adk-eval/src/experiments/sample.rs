//! Cumulative per-arm samples and looks (spec 3.5 §7.12).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::config::MetricKind;
use crate::error::{EvalError, Result};

/// The cumulative data of one metric in one arm (spec 3.5 §7.12).
///
/// Samples are cumulative from the start of the experiment: each look carries the totals so far,
/// never one day's increment, so sending the same look twice changes nothing. [`Sample::merge`]
/// adds an increment to a running total.
///
/// # Example
///
/// ```
/// use adk_eval::experiments::Sample;
///
/// let monday = Sample::proportion(500, 20);
/// let tuesday = Sample::proportion(480, 25);
/// let total = monday.merge(&tuesday).unwrap();
/// assert_eq!(total, Sample::proportion(980, 45));
///
/// let revenue = Sample::from_values(&[0.0, 12.5, 0.0, 30.0]);
/// assert_eq!(revenue.units(), 4);
/// assert_eq!(revenue.estimate(), 10.625);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[non_exhaustive]
pub enum Sample {
    /// Units exposed, and how many of them had the event.
    #[serde(rename_all = "camelCase")]
    Proportion {
        /// Units exposed to the arm.
        units: u64,
        /// Units with at least one event; at most `units`.
        events: u64,
    },
    /// Units exposed, and the mean and sample variance of their per-unit values.
    #[serde(rename_all = "camelCase")]
    Mean {
        /// Units exposed to the arm.
        units: u64,
        /// The mean per-unit value.
        mean: f64,
        /// The sample variance of the per-unit values (denominator `units - 1`).
        variance: f64,
    },
}

impl Sample {
    /// Creates a proportion sample: `events` of `units` had the event.
    pub fn proportion(units: u64, events: u64) -> Self {
        Self::Proportion { units, events }
    }

    /// Creates a mean sample from its summary statistics.
    pub fn mean(units: u64, mean: f64, variance: f64) -> Self {
        Self::Mean { units, mean, variance }
    }

    /// Summarises per-unit values as a mean sample.
    pub fn from_values(values: &[f64]) -> Self {
        // Welford's update keeps the variance accurate when the mean is large.
        let mut mean = 0.0;
        let mut m2 = 0.0;
        for (index, value) in values.iter().enumerate() {
            let delta = value - mean;
            mean += delta / (index + 1) as f64;
            m2 += delta * (value - mean);
        }
        let units = values.len() as u64;
        let variance = if units > 1 { m2 / (units - 1) as f64 } else { 0.0 };
        Self::Mean { units, mean, variance }
    }

    /// Returns how the sample is measured.
    pub fn kind(&self) -> MetricKind {
        match self {
            Self::Proportion { .. } => MetricKind::Proportion,
            Self::Mean { .. } => MetricKind::Mean,
        }
    }

    /// Returns the units exposed.
    pub fn units(&self) -> u64 {
        match *self {
            Self::Proportion { units, .. } | Self::Mean { units, .. } => units,
        }
    }

    /// Returns the rate of a proportion or the mean of a mean sample; `0.0` with no units.
    pub fn estimate(&self) -> f64 {
        match *self {
            Self::Proportion { units: 0, .. } => 0.0,
            Self::Proportion { units, events } => events as f64 / units as f64,
            Self::Mean { mean, .. } => mean,
        }
    }

    /// Returns the variance of the estimate: `p(1 − p)/n` or `s²/n`; `0.0` with no units.
    pub(crate) fn variance_of_estimate(&self) -> f64 {
        match *self {
            Self::Proportion { units: 0, .. } | Self::Mean { units: 0, .. } => 0.0,
            Self::Proportion { units, .. } => {
                let rate = self.estimate();
                rate * (1.0 - rate) / units as f64
            }
            Self::Mean { units, variance, .. } => variance / units as f64,
        }
    }

    /// Adds an increment of the same kind to this sample.
    ///
    /// Mean samples combine with the parallel update of Chan, Golub and LeVeque, so merging
    /// daily summaries gives the same mean and variance as summarising every value at once.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::StatisticsError`] when the samples are of different kinds or either
    /// is invalid.
    pub fn merge(&self, other: &Sample) -> Result<Sample> {
        self.check("merge")?;
        other.check("merge")?;
        match (*self, *other) {
            (
                Self::Proportion { units: left_units, events: left_events },
                Self::Proportion { units: right_units, events: right_events },
            ) => Ok(Self::Proportion {
                units: left_units.saturating_add(right_units),
                events: left_events.saturating_add(right_events),
            }),
            (
                Self::Mean { units: left_units, mean: left_mean, variance: left_variance },
                Self::Mean { units: right_units, mean: right_mean, variance: right_variance },
            ) => {
                let units = left_units.saturating_add(right_units);
                if units == 0 {
                    return Ok(Self::Mean { units: 0, mean: 0.0, variance: 0.0 });
                }
                let (left_n, right_n, n) = (left_units as f64, right_units as f64, units as f64);
                let delta = right_mean - left_mean;
                let mean = left_mean + delta * right_n / n;
                let left_m2 = left_variance * (left_n - 1.0).max(0.0);
                let right_m2 = right_variance * (right_n - 1.0).max(0.0);
                let m2 = left_m2 + right_m2 + delta * delta * left_n * right_n / n;
                let variance = if units > 1 { m2 / (n - 1.0) } else { 0.0 };
                Ok(Self::Mean { units, mean, variance })
            }
            (left, right) => Err(EvalError::StatisticsError(format!(
                "cannot merge a {:?} sample with a {:?} sample; merge samples of one metric",
                left.kind(),
                right.kind()
            ))),
        }
    }

    /// Checks the sample's values, naming `context` in the error.
    pub(crate) fn check(&self, context: &str) -> Result<()> {
        match *self {
            Self::Proportion { units, events } if events > units => {
                Err(EvalError::StatisticsError(format!(
                    "{context}: {events} events among {units} units; a proportion counts each \
                     unit at most once, so use a mean sample for events per unit"
                )))
            }
            Self::Proportion { .. } => Ok(()),
            Self::Mean { mean, variance, .. } if !mean.is_finite() || !variance.is_finite() => {
                Err(EvalError::StatisticsError(format!(
                    "{context}: mean {mean} and variance {variance} must be finite"
                )))
            }
            Self::Mean { variance, .. } if variance < 0.0 => Err(EvalError::StatisticsError(
                format!("{context}: variance {variance} is negative"),
            )),
            Self::Mean { .. } => Ok(()),
        }
    }
}

/// The samples of one metric in the control and the variant (spec 3.5 §7.12).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArmSamples {
    /// The control arm (A).
    pub control: Sample,
    /// The variant arm (B).
    pub variant: Sample,
}

impl ArmSamples {
    /// Pairs a control sample with a variant sample.
    pub fn new(control: Sample, variant: Sample) -> Self {
        Self { control, variant }
    }

    /// Returns the effective sample size `n_c·n_v / (n_c + n_v)`; `0.0` when either arm is
    /// empty.
    pub(crate) fn effective_units(&self) -> f64 {
        let (control, variant) = (self.control.units() as f64, self.variant.units() as f64);
        if control == 0.0 || variant == 0.0 {
            return 0.0;
        }
        control * variant / (control + variant)
    }
}

/// One look at an experiment: the cumulative samples of every metric so far (spec 3.5 §7.12).
///
/// # Example
///
/// ```
/// use adk_eval::experiments::{Look, Sample};
///
/// let look = Look::new()
///     .with_metric("signups", Sample::proportion(600, 24), Sample::proportion(600, 30))
///     .with_metric("unsubscribes", Sample::proportion(600, 3), Sample::proportion(600, 6));
/// assert_eq!(look.metric("signups").unwrap().variant.units(), 600);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Look {
    /// Samples by metric id.
    pub metrics: BTreeMap<String, ArmSamples>,
}

impl Look {
    /// Creates an empty look.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds the cumulative samples of `metric`.
    pub fn with_metric(
        mut self,
        metric: impl Into<String>,
        control: Sample,
        variant: Sample,
    ) -> Self {
        self.metrics.insert(metric.into(), ArmSamples::new(control, variant));
        self
    }

    /// Returns the samples of `metric`.
    pub fn metric(&self, metric: &str) -> Option<&ArmSamples> {
        self.metrics.get(metric)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merging_daily_summaries_matches_summarising_all_values() {
        let monday = [3.0, 0.0, 12.5, 7.25, 0.0];
        let tuesday = [100.0, 1.0, 2.0];
        let merged = Sample::from_values(&monday).merge(&Sample::from_values(&tuesday)).unwrap();
        let all: Vec<f64> = monday.iter().chain(&tuesday).copied().collect();
        let Sample::Mean { units, mean, variance } = merged else { panic!("mean sample") };
        let Sample::Mean { mean: expected_mean, variance: expected_variance, .. } =
            Sample::from_values(&all)
        else {
            panic!("mean sample")
        };
        assert_eq!(units, 8);
        assert!((mean - expected_mean).abs() < 1e-12);
        assert!((variance - expected_variance).abs() < 1e-9);
    }

    #[test]
    fn invalid_samples_are_refused() {
        assert!(Sample::proportion(10, 11).check("look").is_err());
        assert!(Sample::mean(10, f64::NAN, 1.0).check("look").is_err());
        assert!(Sample::mean(10, 1.0, -1.0).check("look").is_err());
        assert!(Sample::proportion(10, 1).merge(&Sample::mean(10, 1.0, 1.0)).is_err());
    }

    #[test]
    fn effective_units_is_half_the_arm_size_for_equal_arms() {
        let arms = ArmSamples::new(Sample::proportion(1000, 1), Sample::proportion(1000, 2));
        assert_eq!(arms.effective_units(), 500.0);
        let empty = ArmSamples::new(Sample::proportion(0, 0), Sample::proportion(1000, 2));
        assert_eq!(empty.effective_units(), 0.0);
    }
}
