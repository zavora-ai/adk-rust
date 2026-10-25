//! Analysis of web experiments: sequential tests with a fixed error budget, effect estimates
//! with intervals, guardrail monitoring, and an optional Bayesian analysis (spec 3.5 §7.12,
//! A-I15, §13 Q8).
//!
//! `AbComparator` (feature `statistics`) compares two agents case by case with a paired test.
//! A web experiment is different: visitors are split between a control (A) and a variant (B),
//! each visitor sees one of them, and the counts arrive day by day. [`ExperimentAnalyzer`]
//! analyses these unpaired, growing samples and may be consulted after every day without
//! spending more than its error budget.
//!
//! | Part | What it does |
//! |------|--------------|
//! | Primary metric | Two-sided always-valid test of "variant − control = 0" for a proportion or a mean |
//! | Effect estimates | Difference and relative lift with confidence sequences (credible intervals in the Bayesian option) |
//! | Guardrails | One-sided always-valid test of "harm within tolerance" at every look; a breach stops the experiment and keeps the control (A-I15) |
//! | Error budget | `alpha` for the primary metric; `guardrail_alpha` split equally across guardrails |
//! | Bayesian option | Posterior probability that B is better, expected loss and credible intervals for the primary metric |
//!
//! # Method
//!
//! ## The sequential test
//!
//! At each look the analyzer computes, for a metric, the difference `d = B − A` of the arms'
//! estimates (rates or means), its standard error `se` from the unpooled per-arm variances
//! (`p(1 − p)/n` or `s²/n`), `z = d / se`, and the effective sample size
//! `n = n_A·n_B / (n_A + n_B)`. Evidence against "no difference" is the normal-mixture
//! likelihood ratio
//!
//! ```text
//! Λ = (1 + ρ²n)^(−1/2) · exp(ρ²n · z² / (2(1 + ρ²n)))
//! ```
//!
//! the likelihood ratio of "the standardised effect is drawn from N(0, ρ²)" against "it is 0"
//! (Robbins' method of mixtures). Under no difference Λ is a nonnegative martingale that starts
//! at 1, so by Ville's inequality it ever reaches `1/α` with probability at most α — at a
//! single look, at every look, or at a look chosen because the data looked good. The
//! always-valid p-value is the running minimum of `min(1, 1/Λ)` and the test rejects when it
//! reaches α. Inverting the boundary gives the confidence sequence `d ± z*·se` with
//!
//! ```text
//! z*² = (1 + ρ²n) / (ρ²n) · (2 ln(1/α) + ln(1 + ρ²n))
//! ```
//!
//! which covers the true difference at all looks simultaneously with probability `1 − α`, and
//! excludes zero exactly when the test rejects. This is the Gaussian-mixture asymptotic
//! confidence sequence of Waudby-Smith et al. and the mixture sequential probability ratio test
//! of Johari et al. [`SequentialBoundary`] implements it.
//!
//! ρ is fixed before the first look from the planned units per arm `m`:
//! `ρ² = (2 ln(1/α) + ln(1 + 2 ln(1/α))) / (m/2)`, which makes the boundary tightest near the
//! planned size. At that size and α = 0.05 the critical |z| is 3.04, against 1.96 for a single
//! fixed-sample look: the price of looking as often as you like. ρ changes power, never
//! validity, so a wrong plan only costs time.
//!
//! The relative lift `B/A − 1` gets its interval from the same boundary through the delta
//! method.
//!
//! ## Guardrails
//!
//! A guardrail is a metric the variant must not make worse beyond a [`Tolerance`]. For a metric
//! where lower is better (unsubscribes) the harm is `B − (1 + r)·A` with a relative tolerance
//! `r`, or `B − A − m` with an absolute margin `m`; where higher is better, `(1 − r)·A − B` or
//! `A − B − m`. Its standard error combines both arms' variances with the same weights. The
//! guardrail is breached when the always-valid p-value of "harm ≤ 0" reaches its share of the
//! budget, `guardrail_alpha / k` for `k` guardrails — only evidence of harm counts, using the
//! two-sided boundary on one side, which is conservative. The report gives the harm's lower
//! confidence bound, which is above zero exactly when the guardrail is breached.
//!
//! Guardrails are checked before the primary metric at every look and always use the sequential
//! test, whatever the primary metric's method: a breach is a safety stop and does not depend on
//! a prior.
//!
//! ## Decisions
//!
//! | Decision | When |
//! |----------|------|
//! | `KeepControl { GuardrailBreach }` | Any guardrail is breached; this overrides the primary metric (A-I15) |
//! | `KeepControl { VariantWorse }` | The primary test rejects with the variant worse |
//! | `AdoptVariant` | The primary test rejects with the variant better, no guardrail is breached, and every guardrail has reached the minimum units with its estimated harm within tolerance |
//! | `KeepControl { SampleLimitReached }` | Both arms of the primary metric reach `max_units_per_arm` undecided |
//! | `Continue` | Otherwise |
//!
//! Every decision but `Continue` is final; later looks return the same report.
//!
//! A guardrail whose estimate shows harm that is not yet proven holds adoption back: the
//! experiment continues until the harm is proven (and the control kept) or the estimate falls
//! back within tolerance. This condition only ever withholds adoption, so it spends no error
//! budget. A guardrail with `Tolerance::Absolute(0.0)` therefore needs the variant to look no
//! worse than the control at the adopting look.
//!
//! ## The Bayesian option
//!
//! With [`AnalysisMethod::Bayesian`], the primary metric is judged by its posterior instead of
//! the sequential test. A proportion's rate in each arm has a `Beta(a + events, b + non-events)`
//! posterior from a [`BetaPrior`] (`Beta(1, 1)` by default); a mean has a flat prior and a
//! normal posterior `N(x̄, s²/n)`. The posterior of the difference is approximated as normal
//! from the arms' posterior means and variances, which is accurate once each arm has a few
//! dozen events. The report gives `P(B better)`, the expected loss of adopting B and of keeping
//! A in the metric's units, and a credible interval. The variant is adopted when
//! `P(B better)` reaches `decision_probability` (and, when set, the expected loss of adopting
//! it is at most `max_expected_loss`); the control is kept when `P(B better)` falls to
//! `1 − decision_probability` under the same loss rule.
//!
//! > **Note:** the Bayesian option has no type I error guarantee when it is looked at
//! > repeatedly. It answers "how likely is B better, and what does choosing it risk", not "is
//! > the difference real at a fixed error rate". The sequential test is the default for that
//! > reason (spec 3.5 §13 Q8).
//!
//! # Assumptions
//!
//! - **Randomised, stable assignment.** Each unit (an anonymous bucket) is assigned to one arm
//!   by a stable hash and stays there; the unit of analysis is the unit of assignment.
//! - **Independent units, no interference.** One unit's outcome does not depend on another's
//!   arm.
//! - **A fixed allocation ratio.** The split between arms does not change during the
//!   experiment; ramping traffic changes the mix of days in each arm.
//! - **Cumulative looks.** Each look carries the totals since the start; the analyzer refuses a
//!   look with fewer units or events than the previous one.
//! - **Large-sample normality.** Validity is asymptotic: the per-arm estimates must be close to
//!   normal. No test decides before each arm has `min_units_per_arm` units (default 100), and
//!   the boundary is wide at small sizes. Heavy-tailed means such as revenue per visitor need
//!   more units than rates do.
//! - **Metrics and guardrails fixed in advance.** Adding a metric after seeing data, or
//!   restarting an experiment and keeping its data, spends budget the analysis does not
//!   account for. [`ExperimentAnalyzer::resume`] refuses a changed guardrail set.
//! - **Average effects.** The test detects a difference in the average outcome over the
//!   exposure period; novelty effects and effects that change over time are averaged.
//!
//! # References
//!
//! - Johari, Koomen, Pekelis and Walsh, "Always valid inference: continuous monitoring of A/B
//!   tests", *Operations Research* 70(3), 2022.
//! - Howard, Ramdas, McAuliffe and Sekhon, "Time-uniform, nonparametric, nonasymptotic
//!   confidence sequences", *Annals of Statistics* 49(2), 2021.
//! - Waudby-Smith, Arbour, Sinha, Kennedy and Ramdas, "Time-uniform central limit theory and
//!   asymptotic confidence sequences", arXiv:2103.06476.
//!
//! # Example
//!
//! ```
//! use adk_eval::experiments::{
//!     Decision, ExperimentAnalyzer, ExperimentConfig, Look, MetricDirection, MetricSpec, Sample,
//! };
//!
//! let config = ExperimentConfig::new(
//!     "checkout-button",
//!     MetricSpec::proportion("orders", MetricDirection::HigherIsBetter),
//! );
//! let mut analyzer = ExperimentAnalyzer::new(config).unwrap();
//!
//! // Day 1: too early to tell.
//! let day_one = Look::new().with_metric(
//!     "orders",
//!     Sample::proportion(1_000, 100),
//!     Sample::proportion(1_000, 115),
//! );
//! let report = analyzer.observe(&day_one).unwrap();
//! assert_eq!(report.decision, Decision::Continue);
//!
//! // Day 7: the totals so far.
//! let day_seven = Look::new().with_metric(
//!     "orders",
//!     Sample::proportion(7_000, 700),
//!     Sample::proportion(7_000, 840),
//! );
//! let report = analyzer.observe(&day_seven).unwrap();
//! assert_eq!(report.decision, Decision::AdoptVariant);
//! let interval = report.primary.effect.difference_interval.unwrap();
//! assert!(interval.lower > 0.0 && interval.contains(0.02));
//! println!("{report}");
//! ```

mod analyzer;
mod config;
mod normal;
mod report;
mod sample;
mod sequential;

pub use analyzer::{ExperimentAnalyzer, ExperimentState};
pub use config::{
    AnalysisMethod, BayesianConfig, BetaPrior, DEFAULT_ALPHA, DEFAULT_GUARDRAIL_ALPHA,
    DEFAULT_MIN_UNITS_PER_ARM, DEFAULT_PLANNED_UNITS_PER_ARM, ErrorBudget, ExperimentConfig,
    GuardrailSpec, MetricDirection, MetricKind, MetricSpec, Tolerance,
};
pub use report::{
    Decision, EffectEstimate, Evidence, ExperimentReport, GuardrailAnalysis, Interval,
    PrimaryAnalysis, StopReason,
};
pub use sample::{ArmSamples, Look, Sample};
pub use sequential::SequentialBoundary;
