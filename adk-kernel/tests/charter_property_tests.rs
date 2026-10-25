//! Property tests of the charter module (spec 3.5 §7.10, §7.11, §11), 128 cases each: KPI
//! aggregates have no gap and no duplicate per period; signals of one kind and topic file
//! one item and count every repeat; the autonomy ladder never rises without an owner's
//! decision; an approval chain's verdict does not depend on the order of its approvals.

#![cfg(feature = "charter")]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use adk_core::principal::{SubjectKind, SubjectRef};
use adk_kernel::charter::{
    AutonomyLadder, InMemoryAutonomyStore, InMemoryKpiStore, InMemorySignalStore, KpiPeriod,
    KpiSample, KpiStore, OwnerDecision, RoleApproval, SignalOutcome, SignalRateLimit, SignalSink,
    check_chain, ladder_rank, period_start, signal_key,
};
use adk_kernel::conformance::charter::fixtures::{
    charter, content_gap, review, with_chain, with_role,
};
use adk_kernel::{AgendaFilter, AgendaOrigin, AgendaStore, InMemoryAgendaStore, PartitionId};
use awp_types::{AutonomyLevel, Role};
use chrono::{Duration, NaiveDate, TimeZone, Utc};
use proptest::prelude::*;
use uuid::Uuid;

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// Daily and weekly aggregates over any range hold exactly one bucket per period, in
    /// order, and every recorded sample in its period exactly once.
    #[test]
    fn kpi_aggregates_have_no_gap_and_no_duplicate(
        samples in proptest::collection::vec((0_i64..60 * 24 * 60, -1_000.0_f64..1_000.0), 0..80),
        from_day in 0_i64..60,
        span in 0_i64..60,
        weekly in any::<bool>(),
    ) {
        let period = if weekly { KpiPeriod::Week } else { KpiPeriod::Day };
        let origin = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).single().unwrap();
        let site = Uuid::nil();
        let store = InMemoryKpiStore::new();
        let mut kept: BTreeMap<i64, f64> = BTreeMap::new();
        runtime().block_on(async {
            for (minute, value) in &samples {
                let at = origin + Duration::minutes(*minute);
                let fresh = store.record(KpiSample::new(site, "visits", at, *value, "beacon")).await.unwrap();
                prop_assert_eq!(fresh, !kept.contains_key(minute), "one sample per instant");
                kept.entry(*minute).or_insert(*value);
            }
            let from = origin.date_naive() + Duration::days(from_day);
            let to = from + Duration::days(span);
            let series = store.aggregates(site, "visits", period, from, to).await.unwrap();
            let first = period_start(period, from);
            let expected = (period_start(period, to) - first).num_days() / match period {
                KpiPeriod::Week => 7,
                _ => 1,
            } + 1;
            prop_assert_eq!(series.len() as i64, expected, "one bucket per period");
            for (index, bucket) in series.iter().enumerate() {
                let start = first + Duration::days(index as i64 * if weekly { 7 } else { 1 });
                prop_assert_eq!(bucket.start, start, "no gap");
            }
            let last = period_start(period, to) + Duration::days(if weekly { 7 } else { 1 });
            let mut by_start: BTreeMap<NaiveDate, (u64, f64)> = BTreeMap::new();
            for (minute, value) in &kept {
                let day = (origin + Duration::minutes(*minute)).date_naive();
                let start = period_start(period, day);
                if start >= first && start < last {
                    let entry = by_start.entry(start).or_default();
                    entry.0 += 1;
                    entry.1 += value;
                }
            }
            let counted: u64 = series.iter().map(|b| b.count).sum();
            prop_assert_eq!(counted, by_start.values().map(|(n, _)| n).sum::<u64>(), "each sample once");
            for bucket in &series {
                let (count, sum) = by_start.get(&bucket.start).copied().unwrap_or_default();
                prop_assert_eq!(bucket.count, count);
                prop_assert!((bucket.sum - sum).abs() < 1e-6);
            }
            Ok(())
        })?;
    }

    /// Signals of one kind and topic — however the topic is cased and spaced — file one
    /// agenda item whose count is the number of signals; other topics file their own.
    #[test]
    fn signals_of_one_kind_and_topic_count_on_one_item(
        picks in proptest::collection::vec((0_usize..4, any::<bool>(), 0_usize..3), 1..40),
    ) {
        let topics = ["opening hours", "parking", "returns policy", "delivery"];
        runtime().block_on(async {
            let agenda = Arc::new(InMemoryAgendaStore::new());
            let sink = SignalSink::new(Arc::new(InMemorySignalStore::new()), agenda.clone(), "back-office")
                .with_rate_limit(SignalRateLimit::new(1_000, std::time::Duration::from_secs(60)));
            let mut expected: BTreeMap<(String, String), u32> = BTreeMap::new();
            let mut items: BTreeMap<(String, String), String> = BTreeMap::new();
            for (topic, upper, spaces) in &picks {
                let mut text = topics[*topic].to_string();
                if *upper {
                    text = text.to_uppercase();
                }
                text = text.replace(' ', &" ".repeat(spaces + 1));
                let signal = if *topic == 3 { review("maps", 4, &text) } else { content_gap(&text, 1) };
                let key = signal_key(&PartitionId::new("back-office"), &signal);
                let outcome = sink.emit(signal).await.unwrap();
                let count = expected.entry((key.kind.clone(), key.topic.clone())).or_default();
                *count += 1;
                let item = outcome.item().unwrap().to_string();
                let known = items.entry((key.kind, key.topic)).or_insert_with(|| item.clone());
                prop_assert_eq!(&item, known, "a repeat counts on its item");
                match outcome {
                    SignalOutcome::Filed { count: filed, .. } => prop_assert_eq!((filed, *count), (1, 1)),
                    SignalOutcome::Counted { count: counted, .. } => prop_assert_eq!(counted, *count),
                    other => prop_assert!(false, "unexpected {other:?}"),
                }
            }
            let filed = agenda.list(&AgendaFilter::default().partition("back-office")).await.unwrap();
            prop_assert_eq!(filed.len(), expected.len(), "one item per kind and topic");
            let counts: BTreeSet<u32> = filed
                .iter()
                .map(|item| match &item.origin {
                    AgendaOrigin::Signal { count, .. } => *count,
                    _ => 0,
                })
                .collect();
            prop_assert_eq!(counts, expected.values().copied().collect::<BTreeSet<_>>());
            Ok(())
        })?;
    }

    /// Whatever the site does — breaches, incidents, rejections, lowering itself, promotions
    /// asked for by agents, services or people who own nothing — a job's level never rises
    /// unless an owner's decision raised it.
    #[test]
    fn the_ladder_never_rises_without_an_owner_decision(
        start in 0_usize..5,
        steps in proptest::collection::vec((0_usize..7, 0_usize..4), 1..40),
    ) {
        let levels = [
            AutonomyLevel::Observe,
            AutonomyLevel::Propose,
            AutonomyLevel::ActWithApproval,
            AutonomyLevel::ActReversibleOnly,
            AutonomyLevel::ActWithinBudget,
        ];
        runtime().block_on(async {
            let site = Uuid::new_v4();
            let charters = Arc::new(adk_kernel::charter::InMemoryCharterStore::new());
            adk_kernel::charter::CharterStore::commit(
                charters.as_ref(),
                charter(site, &["amina"]),
                adk_kernel::conformance::charter::fixtures::decision("v1", Utc::now(), &[(Role::Owner, "amina")]),
            )
            .await
            .unwrap();
            let ladder = AutonomyLadder::new(Arc::new(InMemoryAutonomyStore::new())).with_charters(charters);
            ladder.register(site, "job", levels[start], Some("serve")).await.unwrap();
            let deciders = [
                SubjectRef::human("amina"),
                SubjectRef::new(SubjectKind::Agent, "urn:adk:agent:site/steward"),
                SubjectRef::service("kernel:autonomy"),
                SubjectRef::human("mallory"),
            ];
            for (index, (action, who)) in steps.iter().enumerate() {
                let before = ladder.level(site, "job").await.unwrap().unwrap();
                let owner_promoted = match action {
                    0 => {
                        let decision = OwnerDecision::new(format!("d-{index}"), deciders[*who].clone(), Utc::now());
                        ladder.promote(site, "job", &decision).await.is_ok()
                    }
                    1 => { ladder.guardrail_breach(site, "job", "g").await.unwrap(); false }
                    2 => { ladder.incident(site, "serve", "i").await.unwrap(); false }
                    3 => { let _ = ladder.lower(site, "job", "self").await.unwrap(); false }
                    4 => {
                        let decision = adk_kernel::charter::JobDecision::new(
                            site, "job", format!("j-{index}"),
                            adk_kernel::charter::DecisionOutcome::Rejected,
                            deciders[*who].clone(), Utc::now(),
                        );
                        let _ = ladder.record_decision(decision).await;
                        false
                    }
                    5 => { ladder.offers(site).await.unwrap(); false }
                    _ => {
                        let decision = OwnerDecision::new(format!("d-{index}"), deciders[*who].clone(), Utc::now());
                        let to = levels[(ladder_rank(before) + 2).min(4)];
                        ladder.promote_to(site, "job", to, &decision).await.is_ok()
                    }
                };
                let after = ladder.level(site, "job").await.unwrap().unwrap();
                if ladder_rank(after) > ladder_rank(before) {
                    prop_assert!(owner_promoted && *who == 0, "rose from {before:?} to {after:?} without the owner");
                }
            }
            Ok(())
        })?;
    }

    /// An approval chain's verdict does not depend on the order of its approvals.
    #[test]
    fn a_chains_verdict_ignores_the_order_of_approvals(
        approvals in proptest::collection::vec((0_usize..3, 0_usize..4), 0..6),
        rotate in 0_usize..6,
    ) {
        let roles = [Role::Editor, Role::Publisher, Role::Owner];
        let people = ["eva", "paul", "amina", "urn:adk:agent:site/growth"];
        let governance = with_chain(
            with_role(with_role(with_role(charter(Uuid::nil(), &["amina", "paul"]), Role::Editor, "eva"), Role::Publisher, "paul"), Role::Publisher, "eva"),
            "publish",
            &[Role::Editor, Role::Publisher, Role::Owner],
        )
        .governance;
        let now = Utc::now();
        let publish = awp_types::ActionClass::Publish;
        let mut list: Vec<RoleApproval> = approvals
            .iter()
            .map(|(role, who)| RoleApproval::new(roles[*role], SubjectRef::human(people[*who]), now))
            .collect();
        let verdict = check_chain(&governance, &publish, &list, None).is_ok();
        if !list.is_empty() {
            let by = rotate % list.len();
            list.rotate_left(by);
        }
        prop_assert_eq!(check_chain(&governance, &publish, &list, None).is_ok(), verdict);
        list.reverse();
        prop_assert_eq!(check_chain(&governance, &publish, &list, None).is_ok(), verdict);
        if verdict {
            let distinct: BTreeSet<&str> = list.iter().map(|a| a.principal.id.as_str()).collect();
            prop_assert!(distinct.len() >= 3, "three roles need three people");
            prop_assert!(!distinct.contains("urn:adk:agent:site/growth"), "no agent approves");
        }
    }
}
