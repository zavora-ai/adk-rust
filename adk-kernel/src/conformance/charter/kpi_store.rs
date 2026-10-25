//! Conformance cases for [`KpiStore`] backends (spec 3.5 §7.11, §11).
//!
//! | Case | Contract |
//! |------|----------|
//! | [`samples_round_trip_in_order`] | Samples read back to the microsecond, oldest first, over `[from, to)`; KPIs and sites are apart |
//! | [`one_sample_per_instant`] | A second sample at the same instant is refused and the first kept |
//! | [`refuses_a_value_that_is_not_finite`] | NaN and infinity are refused |
//! | [`aggregates_have_no_gaps`] | Daily and weekly aggregates hold one entry per period, empty periods included |
//! | [`states_round_trip`] | KPI and guardrail states read back and are replaced |
//! | [`concurrent_records_keep_one_per_instant`] | Concurrent records from two handles keep exactly one sample per instant |
//! | [`samples_survive_a_restart`] | Samples and states read back through a new handle (durable backends only) |

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use awp_types::Measurement;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use uuid::Uuid;

use crate::KernelError;
use crate::charter::{GuardrailState, KpiPeriod, KpiSample, KpiState, KpiStore};

/// Opens a [`KpiStore`] backend for the conformance suite (spec 3.5 §11).
#[async_trait]
pub trait KpiStoreHarness: Send + Sync {
    /// Opens a store. For a durable backend every call returns a new handle over the same
    /// stored data. Panics when the backend cannot be opened.
    async fn open(&self) -> Arc<dyn KpiStore>;

    /// Whether data written through one handle is visible through the next.
    fn is_durable(&self) -> bool {
        true
    }
}

/// An instant with microseconds on 20 October 2026 plus `hours`.
fn at(hours: i64, micros: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 20, 0, 0, 0).single().expect("valid")
        + Duration::hours(hours)
        + Duration::microseconds(micros)
}

/// Samples read back to the microsecond, oldest first, over `[from, to)`.
pub async fn samples_round_trip_in_order(harness: &impl KpiStoreHarness) {
    let store = harness.open().await;
    let (site, other) = (Uuid::new_v4(), Uuid::new_v4());
    for (hours, value) in [(5, 2.5), (1, 1.25), (3, -0.75)] {
        let sample = KpiSample::new(site, "leads/ü q", at(hours, 123), value, "commerce");
        assert!(store.record(sample).await.expect("record"));
    }
    store.record(KpiSample::new(site, "orders", at(2, 0), 9.0, "commerce")).await.expect("record");
    store
        .record(KpiSample::new(other, "leads/ü q", at(2, 0), 7.0, "commerce"))
        .await
        .expect("record");
    let read = store.samples(site, "leads/ü q", at(0, 0), at(24, 0)).await.expect("samples");
    let values: Vec<(DateTime<Utc>, f64)> = read.iter().map(|s| (s.at, s.value)).collect();
    assert_eq!(values, [(at(1, 123), 1.25), (at(3, 123), -0.75), (at(5, 123), 2.5)]);
    assert_eq!(read[0], KpiSample::new(site, "leads/ü q", at(1, 123), 1.25, "commerce"));
    let window = store.samples(site, "leads/ü q", at(1, 123), at(5, 123)).await.expect("samples");
    assert_eq!(window.len(), 2, "from is inclusive, to exclusive");
    assert!(
        store.samples(site, "leads/ü q", at(5, 0), at(1, 0)).await.expect("samples").is_empty()
    );
}

/// A second sample at the same instant is refused and the first kept.
pub async fn one_sample_per_instant(harness: &impl KpiStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    assert!(store.record(KpiSample::new(site, "leads", at(1, 7), 1.0, "a")).await.expect("record"));
    assert!(
        !store.record(KpiSample::new(site, "leads", at(1, 7), 2.0, "b")).await.expect("record")
    );
    let read = store.samples(site, "leads", at(0, 0), at(2, 0)).await.expect("samples");
    assert_eq!(read, vec![KpiSample::new(site, "leads", at(1, 7), 1.0, "a")]);
}

/// NaN and infinity are refused.
pub async fn refuses_a_value_that_is_not_finite(harness: &impl KpiStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let error = store
            .record(KpiSample::new(site, "leads", at(1, 0), value, "a"))
            .await
            .expect_err("not finite");
        assert!(matches!(error, KernelError::Config { .. }), "{error}");
    }
    assert!(store.samples(site, "leads", at(0, 0), at(2, 0)).await.expect("samples").is_empty());
}

/// Daily and weekly aggregates hold one entry per period, empty periods included.
pub async fn aggregates_have_no_gaps(harness: &impl KpiStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    // Tuesday 20 October 2026: two samples; Thursday: one; the next Monday: one.
    for (hours, value) in [(9, 2.0), (18, 4.0), (57, 6.0), (6 * 24 + 1, 8.0)] {
        store
            .record(KpiSample::new(site, "leads", at(hours, 0), value, "a"))
            .await
            .expect("record");
    }
    let day = |d| NaiveDate::from_ymd_opt(2026, 10, d).expect("valid");
    let days =
        store.aggregates(site, "leads", KpiPeriod::Day, day(19), day(27)).await.expect("days");
    let starts: Vec<NaiveDate> = days.iter().map(|d| d.start).collect();
    assert_eq!(starts, (19..=27).map(day).collect::<Vec<_>>(), "one entry per day");
    let counts: Vec<u64> = days.iter().map(|d| d.count).collect();
    assert_eq!(counts, [0, 2, 0, 1, 0, 0, 0, 1, 0]);
    assert_eq!(
        (days[1].sum, days[1].min, days[1].max, days[1].last),
        (6.0, Some(2.0), Some(4.0), Some(4.0))
    );
    assert!(days.iter().all(|d| d.kpi == "leads" && d.period == KpiPeriod::Day));
    let weeks =
        store.aggregates(site, "leads", KpiPeriod::Week, day(21), day(28)).await.expect("weeks");
    let summary: Vec<(NaiveDate, u64, f64)> =
        weeks.iter().map(|w| (w.start, w.count, w.sum)).collect();
    assert_eq!(summary, [(day(19), 3, 12.0), (day(26), 1, 8.0)]);
    assert_eq!(weeks[0].mean(), Some(4.0));
}

/// KPI and guardrail states read back and are replaced.
pub async fn states_round_trip(harness: &impl KpiStoreHarness) {
    let store = harness.open().await;
    let site = Uuid::new_v4();
    assert!(store.state(site, "leads").await.expect("state").is_none());
    assert!(store.guardrail(site, "leads").await.expect("guardrail").is_none());
    let mut state = KpiState::new(site, "leads", at(0, 1));
    store.put_state(state.clone()).await.expect("put");
    assert_eq!(store.state(site, "leads").await.expect("state"), Some(state.clone()));
    state.baseline = Some(Measurement {
        value: 4.5,
        window: std::time::Duration::from_secs(14 * 24 * 60 * 60),
        measured_at: at(14 * 24, 0),
    });
    state.last_read = Some(at(14 * 24, 0));
    store.put_state(state.clone()).await.expect("put");
    assert_eq!(store.state(site, "leads").await.expect("state"), Some(state));
    let mut guard = GuardrailState::new(site, "leads", at(1, 0));
    guard.breached_since = Some(at(1, 0));
    guard.last_value = Some(0.031);
    store.put_guardrail(guard.clone()).await.expect("put");
    assert_eq!(store.guardrail(site, "leads").await.expect("guardrail"), Some(guard));
    assert!(
        store.state(Uuid::new_v4(), "leads").await.expect("state").is_none(),
        "sites are apart"
    );
}

/// Concurrent records from two handles keep exactly one sample per instant.
pub async fn concurrent_records_keep_one_per_instant(harness: &impl KpiStoreHarness) {
    let first = harness.open().await;
    let second = if harness.is_durable() { harness.open().await } else { Arc::clone(&first) };
    let site = Uuid::new_v4();
    let mut tasks = Vec::new();
    for index in 0..40_i64 {
        let store = if index % 2 == 0 { Arc::clone(&first) } else { Arc::clone(&second) };
        // Every instant is written twice, once through each handle.
        let instant = at(index / 2, 0);
        tasks.push(tokio::spawn(async move {
            store.record(KpiSample::new(site, "visits", instant, index as f64, "beacon")).await
        }));
    }
    let mut kept = 0;
    for task in tasks {
        if task.await.expect("task").expect("record") {
            kept += 1;
        }
    }
    assert_eq!(kept, 20, "one per instant");
    let read = first.samples(site, "visits", at(0, 0), at(48, 0)).await.expect("samples");
    let instants: BTreeSet<DateTime<Utc>> = read.iter().map(|s| s.at).collect();
    assert_eq!((read.len(), instants.len()), (20, 20));
}

/// Samples and states read back through a new handle.
pub async fn samples_survive_a_restart(harness: &impl KpiStoreHarness) {
    if !harness.is_durable() {
        return;
    }
    let site = Uuid::new_v4();
    let writer = harness.open().await;
    writer.record(KpiSample::new(site, "leads", at(1, 9), 3.0, "a")).await.expect("record");
    writer.put_state(KpiState::new(site, "leads", at(0, 0))).await.expect("put");
    let reader = harness.open().await;
    assert_eq!(reader.samples(site, "leads", at(0, 0), at(2, 0)).await.expect("samples").len(), 1);
    assert!(reader.state(site, "leads").await.expect("state").is_some());
}
