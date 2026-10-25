# Web Experiments

The `experiments` feature adds `ExperimentAnalyzer` to adk-eval: the analysis of an
experiment that splits visitors between a control (A) and a variant (B) and counts
outcomes day by day. It runs a sequential test with a fixed error budget for
proportions and means, reports effects with intervals, monitors guardrail metrics at
every look, and offers a Bayesian analysis as an option. A guardrail breach stops the
experiment and keeps the control.

`AbComparator` (feature `statistics`) remains the tool for comparing two agents on one
eval set: it pairs their scores case by case. A web experiment has no pairs — each
visitor sees one arm — and its data grow with every look, so it needs an unpaired
sequential test.

## Setup

```toml
[dependencies]
adk-eval = { version = "3.0.0", features = ["experiments"] }
```

The feature adds one dependency, `libm`, for the normal distribution in the Bayesian
analysis.

## Running an experiment

1. Describe the experiment: an id, the primary metric, and the guardrails.
2. At each look, pass the cumulative totals of every metric in both arms.
3. Act on the decision; save the analyzer's state between looks.

```rust
use adk_eval::experiments::{
    Decision, ExperimentAnalyzer, ExperimentConfig, GuardrailSpec, Look, MetricDirection,
    MetricSpec, Sample, Tolerance,
};

fn main() -> adk_eval::Result<()> {
    let config = ExperimentConfig::new(
        "headline",
        MetricSpec::proportion("signups", MetricDirection::HigherIsBetter),
    )
    .with_guardrail(GuardrailSpec::new(
        MetricSpec::proportion("unsubscribes", MetricDirection::LowerIsBetter),
        Tolerance::Relative(0.1),
    ));
    let mut analyzer = ExperimentAnalyzer::new(config)?;

    // 600 visitors per arm per day. B lifts sign-ups from 4% to 5% but doubles
    // unsubscribes from 0.5% to 1%.
    for day in 1..=28 {
        let visitors = 600 * day;
        let look = Look::new()
            .with_metric(
                "signups",
                Sample::proportion(visitors, 24 * day),
                Sample::proportion(visitors, 30 * day),
            )
            .with_metric(
                "unsubscribes",
                Sample::proportion(visitors, 3 * day),
                Sample::proportion(visitors, 6 * day),
            );
        let report = analyzer.observe(&look)?;
        if report.decision != Decision::Continue {
            println!("{report}");
            break;
        }
    }
    Ok(())
}
```

The experiment stops at day 11, before the sign-up lift is conclusive, because the
unsubscribe guardrail is breached:

```text
Experiment 'headline', look 11: keep the control — guardrail breached: unsubscribes
Primary signups (higher is better): control 4.00% of 6600, variant 5.00% of 6600
  difference +1.00 pp, 95% interval [-0.10 pp, +2.10 pp]; relative lift +25.0%, 95% interval [-5.8%, +55.8%]
  always-valid p 0.0992 against alpha 0.05; z 2.77 against 3.05
Guardrail unsubscribes (lower is better, tolerance 10.0% relative): control 0.50% of 6600, variant 1.00% of 6600
  difference +0.50 pp, 90% interval [+0.08 pp, +0.92 pp]; relative lift +100.0%, 90% interval [-18.3%, +218.3%]
  harm beyond tolerance +0.45 pp, lower bound +0.02 pp; always-valid p 0.0769 against alpha 0.1000: breached
```

This is scenario S11 of the agentic web spec; `adk-eval/tests/experiments_s11_tests.rs`
runs it, along with the counterfactual in which B leaves unsubscribes alone and is
adopted on day 14.

### Metrics and samples

| Type | Purpose |
|------|---------|
| `MetricSpec::proportion(id, direction)` | The share of units with an event: sign-ups, orders, unsubscribes |
| `MetricSpec::mean(id, direction)` | The mean of a per-unit value: revenue per visitor, pages per session |
| `MetricDirection` | `HigherIsBetter` or `LowerIsBetter`; the charter's KPI direction `Up` and `Down` |
| `Sample::proportion(units, events)` | Cumulative units and units with the event in one arm |
| `Sample::mean(units, mean, variance)`, `Sample::from_values` | Cumulative units and the summary of their values |
| `Sample::merge` | Adds a day's increment to a running total; mean samples merge exactly |
| `Look` | One look: the samples of every metric, by metric id |

Each look carries totals since the start, never one day's increment, so repeating a
look changes nothing. A look that misses a configured metric, carries the wrong kind of
sample, or has fewer units or events than the previous look is refused, and the state
stays as it was.

### Configuration

| Setting | Default | Meaning |
|---------|---------|---------|
| `budget.alpha` | 0.05 | Type I error budget of the primary metric's two-sided test |
| `budget.guardrail_alpha` | 0.10 | Type I error budget shared equally by the guardrails |
| `planned_units_per_arm` | 10,000 | Where the boundary is tightest; tunes power, never validity |
| `min_units_per_arm` | 100 | Units each arm needs before a metric's test may decide |
| `max_units_per_arm` | none | Stops undecided experiments, keeping the control |
| `method` | `Sequential` | `AnalysisMethod::Bayesian(BayesianConfig)` for the Bayesian option |

`Tolerance::Relative(r)` lets the variant be up to `r` worse than the control on a
guardrail; `Tolerance::Absolute(m)` allows a margin `m` in the metric's units.

### Decisions

Guardrails are checked first at every look.

| Decision | When |
|----------|------|
| `KeepControl { GuardrailBreach }` | A guardrail's harm beyond its tolerance is proven; this overrides the primary metric |
| `KeepControl { VariantWorse }` | The primary test proves the variant worse |
| `AdoptVariant` | The primary test proves the variant better, no guardrail is breached, and every guardrail has the minimum units with its estimated harm within tolerance |
| `KeepControl { SampleLimitReached }` | Both arms reach `max_units_per_arm` undecided |
| `Continue` | Otherwise |

A final decision is kept: every later look returns the report of the look that
stopped the experiment.

### Saving state between looks

The always-valid p-values are running minima over every look, so a process that looks
once a day saves `analyzer.state()` (camelCase JSON) and continues with
`ExperimentAnalyzer::resume(config, state)`. Resuming with another experiment's state
or a changed set of guardrails is refused.

## Method

### The sequential test

At each look the analyzer computes the difference `d = B − A` of the arms' rates or
means, its standard error `se` from the unpooled per-arm variances, `z = d / se`, and
the effective sample size `n = n_A·n_B / (n_A + n_B)`. The evidence against "no
difference" is the normal-mixture likelihood ratio

```text
Λ = (1 + ρ²n)^(−1/2) · exp(ρ²n · z² / (2(1 + ρ²n)))
```

Under no difference Λ is a nonnegative martingale starting at 1, so by Ville's
inequality it reaches `1/α` with probability at most α, however many looks are taken.
The always-valid p-value is the running minimum of `min(1, 1/Λ)`; the test rejects when
it reaches α. The same boundary gives the confidence sequence `d ± z*·se`,

```text
z*² = (1 + ρ²n) / (ρ²n) · (2 ln(1/α) + ln(1 + ρ²n))
```

which covers the true difference at every look at once with probability `1 − α` and
excludes zero exactly when the test rejects. `ρ² = (2 ln(1/α) + ln(1 + 2 ln(1/α))) / m`
for `m = planned_units_per_arm / 2` puts the tightest point at the planned size, where
`z*` is 3.04 for α = 0.05 — against 1.96 for a single look at a fixed sample. The
relative lift `B/A − 1` gets its interval from the same boundary by the delta method.
`SequentialBoundary` exposes the boundary directly.

### Guardrails

For a guardrail where lower is better, the harm is `B − (1 + r)·A` (relative tolerance)
or `B − A − m` (absolute margin); where higher is better, `(1 − r)·A − B` or
`A − B − m`. A guardrail is breached when the always-valid p-value of "harm ≤ 0" reaches
`guardrail_alpha / k` for `k` guardrails, which is when the harm's lower confidence
bound rises above zero. Only evidence of harm counts, using the two-sided boundary on
one side, which is conservative. Guardrails use the sequential test whatever the primary
metric's method: a breach is a safety stop and does not depend on a prior.

### The Bayesian option

```rust
use adk_eval::experiments::{AnalysisMethod, BayesianConfig, ExperimentConfig, MetricDirection, MetricSpec};

fn bayesian() -> ExperimentConfig {
    ExperimentConfig::new(
        "headline",
        MetricSpec::proportion("signups", MetricDirection::HigherIsBetter),
    )
    .with_method(AnalysisMethod::Bayesian(
        BayesianConfig::default().with_max_expected_loss(0.001),
    ))
}
```

A proportion's rate has a `Beta(a + events, b + non-events)` posterior from a
`BetaPrior` (`Beta(1, 1)` by default); a mean has a flat prior. The posterior of the
difference is approximated as normal from the arms' posterior moments. The report gives
`P(B better)`, the expected loss of adopting B and of keeping A in the metric's units,
and a credible interval. B is adopted when `P(B better)` reaches `decision_probability`
(0.95 by default) and, when `max_expected_loss` is set, the loss of adopting it is at
most that; A is kept when `P(B better)` falls to `1 − decision_probability`.

> **Note:** the Bayesian option has no type I error guarantee under repeated looks.
> With no real difference, deciding at a 0.95 posterior probability at any of thirty
> daily looks decides in about 45% of experiments in the simulation tests. The
> sequential test is the default for that reason.

### Assumptions

| Assumption | Why it matters |
|------------|----------------|
| Units are assigned at random by a stable hash and stay in one arm | The unit of analysis must be the unit of assignment |
| Units are independent | Interference between units biases the difference |
| The allocation ratio is fixed | Ramping traffic changes the mix of days in each arm |
| Looks are cumulative | The analyzer refuses a look with fewer units or events than the last |
| Estimates are close to normal | Validity is asymptotic; `min_units_per_arm` and the wide early boundary guard small samples, and heavy-tailed means need more units |
| Metrics and guardrails are fixed in advance | A metric added after seeing data spends budget the analysis does not count |
| Effects are averages over the exposure period | Novelty effects and effects that change over time are averaged |

## Verification

`adk-eval/tests/experiments_simulation_tests.rs` checks the method by simulation with
fixed seeds, looking every day for 30 days:

| Property | Simulation | Result |
|----------|------------|--------|
| Type I error under daily peeking | 2,000 experiments, 10% conversion, no difference | 1.4% false decisions against a 5% budget; a fixed-sample 5% test checked daily fires in 27% |
| Heavy-tailed means | 1,000 experiments, revenue per visitor with 3% buyers | 0.5% false decisions |
| Rare-event guardrails | 2,000 experiments, 0.5% unsubscribes; a variant exactly at a 10% relative tolerance | 1.2% and 1.6% false breaches against a 10% budget |
| Power on a known lift | 1,000 experiments, 10% to 12%, 500 visitors per arm per day | 99.6% adopt the variant; the interval at the stopping look covers the true lift in 99.9% |
| Guardrail detection | 500 experiments shaped like S11 | 98.4% keep the control on the breach; 0.4% adopt |

Property tests check that the boundary and the p-value agree, that p-values never rise,
that decisions stick, that swapping the arms mirrors the result, and that the interval
excludes zero exactly when the test rejects.

## References

- Johari, Koomen, Pekelis and Walsh, "Always valid inference: continuous monitoring of
  A/B tests", *Operations Research* 70(3), 2022.
- Howard, Ramdas, McAuliffe and Sekhon, "Time-uniform, nonparametric, nonasymptotic
  confidence sequences", *Annals of Statistics* 49(2), 2021.
- Waudby-Smith, Arbour, Sinha, Kennedy and Ramdas, "Time-uniform central limit theory
  and asymptotic confidence sequences", arXiv:2103.06476.

## See also

- [Agent Evaluation](evaluation.md) — the evaluator, criteria, and A/B comparison of agents
