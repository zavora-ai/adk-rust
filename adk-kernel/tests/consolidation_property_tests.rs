//! Property tests of the consolidation tick's gate (spec 3.2 §7.5, §11, K-I11): over random
//! material, clocks, quiet hours and budgets, no tick is admitted inside quiet hours, a tick
//! without a bare schedule is admitted only when new material, the interval and the
//! debounce all hold, and an admitted tick's budget stays within `max_tokens_per_tick`, the
//! ticks' share of each daily cap and what the day has left.

use adk_core::RunBudget;
use adk_kernel::{
    ConsolidationTick, PartitionMode, QuietHours, TickAdmission, TickInputs, TickMaterial,
    TickSpend, admit_tick,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use proptest::prelude::*;

fn at(minutes: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 23, 0, 0, 0).unwrap() + Duration::minutes(minutes)
}

#[derive(Debug, Clone)]
struct Case {
    now: i64,
    mode: PartitionMode,
    last_tick: Option<i64>,
    sessions: usize,
    facts: usize,
    activity: Option<i64>,
    busy: bool,
    quiet: Option<(u32, u32)>,
    cost_cap: Option<u64>,
    token_cap: Option<u64>,
    proactive_spent: u64,
    spent: (u64, u64),
    share: f32,
    max_tokens: u64,
}

fn case() -> impl Strategy<Value = Case> {
    (
        (
            0..3 * 24 * 60i64,
            prop::sample::select(vec![
                PartitionMode::ReadOnly,
                PartitionMode::Plan,
                PartitionMode::Ask,
                PartitionMode::Auto,
            ]),
            prop::option::of(0..3 * 24 * 60i64),
            0..6usize,
            0..40usize,
            prop::option::of(0..3 * 24 * 60i64),
            any::<bool>(),
        ),
        (
            prop::option::of((0..24u32, 0..24u32)),
            prop::option::of(1_000..2_000_000u64),
            prop::option::of(1_000..400_000u64),
            0..2_000_000u64,
            (0..300_000u64, 0..80_000u64),
            prop::sample::select(vec![0.0f32, 0.05, 0.1, 0.5, 1.0]),
            1_000..100_000u64,
        ),
    )
        .prop_map(
            |(
                (now, mode, last_tick, sessions, facts, activity, busy),
                (quiet, cost_cap, token_cap, proactive_spent, spent, share, max_tokens),
            )| Case {
                now,
                mode,
                last_tick,
                sessions,
                facts,
                activity,
                busy,
                quiet,
                cost_cap,
                token_cap,
                proactive_spent,
                spent,
                share,
                max_tokens,
            },
        )
}

fn decide(case: &Case) -> (ConsolidationTick, TickInputs, TickAdmission) {
    let tick = ConsolidationTick::default()
        .with_budget_share(case.share)
        .with_max_tokens_per_tick(case.max_tokens);
    let mut material = TickMaterial::new("alice", case.mode)
        .with_new_material(case.sessions, case.facts)
        .with_spent_today(TickSpend::new(case.spent.0, case.spent.1, 0));
    if let Some(last) = case.last_tick {
        material = material.with_last_tick(at(last));
    }
    if let Some(activity) = case.activity {
        material = material.with_last_activity(at(activity));
    }
    let mut daily = RunBudget::new();
    if let Some(cap) = case.cost_cap {
        daily = daily.max_cost_micro_usd(cap);
    }
    if let Some(cap) = case.token_cap {
        daily = daily.max_total_tokens(cap);
    }
    let windows: Vec<QuietHours> = case
        .quiet
        .map(|(start, end)| {
            format!("{start:02}:00-{end:02}:00")
                .parse::<QuietHours>()
                .unwrap()
                .with_utc_offset_minutes(0)
        })
        .into_iter()
        .collect();
    let inputs = TickInputs::new(at(case.now), material)
        .with_busy(case.busy)
        .with_quiet_hours(windows)
        .with_daily_budget(daily, case.proactive_spent);
    let decision = admit_tick(&tick, &inputs);
    (tick, inputs, decision)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// K-I11: no tick is admitted inside quiet hours.
    #[test]
    fn no_tick_starts_inside_quiet_hours(case in case()) {
        let (_, inputs, decision) = decide(&case);
        if inputs.quiet_hours.iter().any(|window| window.contains(inputs.now)) {
            prop_assert!(!matches!(decision, TickAdmission::Admit { .. }), "{decision:?}");
        }
    }

    /// Without a bare schedule, an admitted tick has new material, the interval passed, no
    /// running process, the debounce passed, and an Ask or Auto partition.
    #[test]
    fn a_tick_is_admitted_only_when_every_gate_holds(case in case()) {
        let (tick, inputs, decision) = decide(&case);
        if matches!(decision, TickAdmission::Admit { .. }) {
            let material = &inputs.material;
            prop_assert!(matches!(material.mode, PartitionMode::Ask | PartitionMode::Auto));
            prop_assert!(
                material.new_sessions >= tick.min_new_sessions
                    || material.new_facts >= tick.min_new_facts
            );
            let interval = Duration::from_std(tick.min_interval).unwrap();
            let debounce = Duration::from_std(tick.debounce).unwrap();
            prop_assert!(material.last_tick.is_none_or(|last| inputs.now - last >= interval));
            prop_assert!(!inputs.busy);
            prop_assert!(material.last_activity.is_none_or(|active| inputs.now - active >= debounce));
        }
    }

    /// An admitted tick's budget stays within the per-tick token cap, the ticks' share of
    /// each daily cap, and what the day has left.
    #[test]
    fn an_admitted_budget_stays_within_the_caps(case in case()) {
        let (tick, inputs, decision) = decide(&case);
        let TickAdmission::Admit { budget } = decision else { return Ok(()) };
        let share = |cap: u64| (cap as f64 * f64::from(tick.budget_share)).floor() as u64;
        let tokens = budget.max_total_tokens.expect("a tick always has a token cap");
        prop_assert!(tokens > 0 && tokens <= tick.max_tokens_per_tick);
        if let Some(cap) = inputs.daily_budget.max_total_tokens {
            prop_assert!(inputs.material.spent_today.total_tokens + tokens <= share(cap));
        }
        if let Some(cap) = inputs.daily_budget.max_cost_micro_usd {
            let cost = budget.max_cost_micro_usd.expect("a daily cost cap caps the tick");
            prop_assert!(cost > 0);
            prop_assert!(inputs.material.spent_today.cost_micro_usd + cost <= share(cap));
            prop_assert!(inputs.proactive_spent + cost <= cap);
        }
    }
}
