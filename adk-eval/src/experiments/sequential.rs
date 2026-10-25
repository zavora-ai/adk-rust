//! The normal-mixture sequential boundary: always-valid p-values and confidence sequences
//! (spec 3.5 §7.12).

use serde::{Deserialize, Serialize};

use crate::error::{EvalError, Result};

/// The always-valid boundary of a sequential test with a fixed error budget (spec 3.5 §7.12).
///
/// For an estimate with standard error `se` after `n` effective units, the boundary rejects
/// "no difference" when `|z| = |estimate / se|` reaches [`critical_z(n)`](Self::critical_z),
///
/// ```text
/// z*(n)² = (1 + ρ²n) / (ρ²n) · (2 ln(1/α) + ln(1 + ρ²n)),
/// ```
///
/// the inversion of the normal-mixture likelihood ratio
/// `Λ = (1 + ρ²n)^(−1/2) · exp(ρ²n·z² / (2(1 + ρ²n)))`. Under no difference Λ is a nonnegative
/// martingale starting at 1, so by Ville's inequality it ever reaches `1/α` with probability at
/// most α, however many looks are taken. `estimate ± z*(n)·se` is the matching confidence
/// sequence: it covers the true difference at every look at once with probability at least
/// `1 − α`.
///
/// ρ is fixed before the first look from the planned effective units `m`:
/// `ρ² = (2 ln(1/α) + ln(1 + 2 ln(1/α))) / m`, which makes the boundary tightest near `m`.
/// It changes power, never validity.
///
/// # Example
///
/// ```
/// use adk_eval::experiments::SequentialBoundary;
///
/// // 10,000 planned units per arm are 5,000 effective units.
/// let boundary = SequentialBoundary::new(0.05, 5_000.0).unwrap();
/// // At the planned size the critical z is 3.04, against 1.96 for one fixed-sample look.
/// assert!((boundary.critical_z(5_000.0) - 3.0352).abs() < 1e-4);
/// // A z on the boundary has an always-valid p-value equal to α.
/// let z = boundary.critical_z(1_200.0);
/// assert!((boundary.p_value(z, 1_200.0) - 0.05).abs() < 1e-12);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SequentialBoundary {
    alpha: f64,
    mixing_rate: f64,
}

impl SequentialBoundary {
    /// Creates the boundary for budget `alpha`, tightest at `planned_effective_units`.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::ConfigError`] when `alpha` is outside `(0, 1)` or the planned size
    /// is not a positive finite number.
    pub fn new(alpha: f64, planned_effective_units: f64) -> Result<Self> {
        if !(alpha > 0.0 && alpha < 1.0) {
            return Err(EvalError::ConfigError(format!(
                "sequential boundary alpha = {alpha}; use a value in (0, 1)"
            )));
        }
        if !(planned_effective_units.is_finite() && planned_effective_units > 0.0) {
            return Err(EvalError::ConfigError(format!(
                "sequential boundary planned size = {planned_effective_units}; use a positive \
                 number of effective units"
            )));
        }
        let c = 2.0 * (1.0 / alpha).ln();
        Ok(Self { alpha, mixing_rate: (c + (1.0 + c).ln()) / planned_effective_units })
    }

    /// Returns the error budget α.
    pub fn alpha(&self) -> f64 {
        self.alpha
    }

    /// Returns ρ², the mixing rate per effective unit.
    pub fn mixing_rate(&self) -> f64 {
        self.mixing_rate
    }

    /// Returns the critical |z| after `effective_units`; infinite with no units.
    pub fn critical_z(&self, effective_units: f64) -> f64 {
        let x = self.mixing_rate * effective_units;
        if x.is_nan() || x <= 0.0 {
            return f64::INFINITY;
        }
        ((1.0 + x) / x * (2.0 * (1.0 / self.alpha).ln() + x.ln_1p())).sqrt()
    }

    /// Returns `min(1, 1/Λ)`, the always-valid p-value of `z` at this look alone.
    ///
    /// The p-value of a test is the running minimum of this value over its looks; it is at most
    /// α exactly when `|z|` reaches [`critical_z`](Self::critical_z).
    pub fn p_value(&self, z: f64, effective_units: f64) -> f64 {
        let x = self.mixing_rate * effective_units;
        if x.is_nan() || x <= 0.0 || !z.is_finite() {
            return 1.0;
        }
        let log_inverse_lambda = 0.5 * x.ln_1p() - x * z * z / (2.0 * (1.0 + x));
        log_inverse_lambda.exp().min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Gaussian-mixture asymptotic confidence sequence of Waudby-Smith, Arbour, Sinha,
    /// Kennedy and Ramdas (arXiv:2103.06476), written in their parameterisation: the half-width
    /// for unit variance after `t` units, times √t.
    fn waudby_smith(alpha: f64, rho2: f64, t: f64) -> f64 {
        (2.0 * (t * rho2 + 1.0) / (t * t * rho2) * ((t * rho2 + 1.0).sqrt() / alpha).ln()).sqrt()
            * t.sqrt()
    }

    #[test]
    fn critical_z_matches_the_published_confidence_sequence() {
        for (alpha, planned, n) in [
            (0.05, 5_000.0, 5_000.0),
            (0.05, 5_000.0, 100.0),
            (0.05, 5_000.0, 50_000.0),
            (0.1, 500.0, 250.0),
            (0.01, 20_000.0, 1_000.0),
        ] {
            let boundary = SequentialBoundary::new(alpha, planned).unwrap();
            let expected = waudby_smith(alpha, boundary.mixing_rate(), n);
            assert!((boundary.critical_z(n) - expected).abs() < 1e-12, "{alpha} {planned} {n}");
        }
    }

    // Reference values computed independently in Python.
    #[test]
    fn critical_z_matches_reference_values() {
        let boundary = SequentialBoundary::new(0.05, 5_000.0).unwrap();
        assert!((boundary.mixing_rate() - 0.001587230920347513).abs() < 1e-15);
        assert!((boundary.critical_z(5_000.0) - 3.0352089903762534).abs() < 1e-12);
        assert!((boundary.critical_z(100.0) - 6.69438861706604).abs() < 1e-12);
        assert!((boundary.critical_z(50_000.0) - 3.241723145405438).abs() < 1e-12);
    }

    #[test]
    fn the_boundary_is_tightest_near_the_planned_size() {
        let boundary = SequentialBoundary::new(0.05, 5_000.0).unwrap();
        let at_plan = boundary.critical_z(5_000.0);
        for n in [500.0, 2_000.0, 20_000.0, 200_000.0] {
            assert!(boundary.critical_z(n) > at_plan, "n = {n}");
        }
        assert!(at_plan > 1.959_963_984_540_054);
    }

    #[test]
    fn no_units_means_no_evidence() {
        let boundary = SequentialBoundary::new(0.05, 5_000.0).unwrap();
        assert_eq!(boundary.critical_z(0.0), f64::INFINITY);
        assert_eq!(boundary.p_value(10.0, 0.0), 1.0);
        assert_eq!(boundary.p_value(0.0, 5_000.0), 1.0);
    }

    #[test]
    fn invalid_settings_are_refused() {
        assert!(SequentialBoundary::new(0.0, 10.0).is_err());
        assert!(SequentialBoundary::new(1.0, 10.0).is_err());
        assert!(SequentialBoundary::new(0.05, 0.0).is_err());
        assert!(SequentialBoundary::new(0.05, f64::INFINITY).is_err());
    }
}
