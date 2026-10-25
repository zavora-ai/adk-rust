//! The standard normal distribution for the Bayesian analysis (spec 3.5 §13 Q8).

use std::f64::consts::{PI, SQRT_2};

/// Returns Φ(x), the standard normal cumulative distribution function.
pub(crate) fn cdf(x: f64) -> f64 {
    0.5 * libm::erfc(-x / SQRT_2)
}

/// Returns φ(x), the standard normal density.
pub(crate) fn pdf(x: f64) -> f64 {
    (-0.5 * x * x).exp() / (2.0 * PI).sqrt()
}

/// Returns Φ⁻¹(p) for `p` in (0, 1).
///
/// Acklam's rational approximation (relative error below 1.2e-9) followed by two Halley steps
/// against [`cdf`], which bring the result to double precision.
pub(crate) fn quantile(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    const LOW: f64 = 0.024_25;

    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    if p > 0.5 {
        // 1 − p is exact for p in [0.5, 1], and the lower tail keeps Φ's precision.
        return -quantile(1.0 - p);
    }
    let mut x = if p < LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    };
    for _ in 0..2 {
        let error = cdf(x) - p;
        let step = error * (2.0 * PI).sqrt() * (0.5 * x * x).exp();
        x -= step / (1.0 + 0.5 * x * step);
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference values from CPython's `math.erfc` and `statistics.NormalDist().inv_cdf`.
    #[test]
    fn cdf_matches_reference_values() {
        let cases = [
            (-8.0, 6.22096057427182e-16),
            (-5.0, 2.866515718791945e-07),
            (-3.0, 0.0013498980316300957),
            (-1.96, 0.02499789514822043),
            (-1.0, 0.15865525393145705),
            (0.0, 0.5),
            (0.5, 0.691462461274013),
            (3.0, 0.9986501019683699),
        ];
        for (x, expected) in cases {
            let actual = cdf(x);
            assert!(
                ((actual - expected) / expected).abs() < 1e-12,
                "Φ({x}) = {actual}, expected {expected}"
            );
        }
    }

    #[test]
    fn quantile_matches_reference_values() {
        let cases = [
            (1e-12, -7.034483825301132),
            (1e-6, -4.753424308822899),
            (0.001, -3.090232306167813),
            (0.02425, -1.9729610513118845),
            (0.025, -1.9599639845400538),
            (0.1, -1.2815515655446006),
            (0.5, 0.0),
            (0.9, 1.2815515655446006),
            (0.975, 1.9599639845400534),
            (0.999, 3.090232306167813),
            (0.999999999, 5.997807019601638),
        ];
        for (p, expected) in cases {
            let actual = quantile(p);
            assert!((actual - expected).abs() < 1e-9, "Φ⁻¹({p}) = {actual}, expected {expected}");
        }
    }

    #[test]
    fn quantile_inverts_cdf() {
        for step in 1..200 {
            let p = step as f64 / 200.0;
            assert!((cdf(quantile(p)) - p).abs() < 1e-14, "p = {p}");
        }
    }
}
