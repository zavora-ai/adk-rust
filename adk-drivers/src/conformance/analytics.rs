//! The analytics and search-console suites (spec 3.5 §7.12, §7.13, §11).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::analytics::{
    AnalyticsDriver, AnalyticsMetrics, MetricQuery, SearchConsoleDriver, SearchFilter,
    SearchMetrics, SearchPerformance,
};
use crate::error::DriverError;
use crate::metrics::KpiSource;

/// Makes fresh analytics adapters for [`analytics_driver_contract`].
#[async_trait]
pub trait AnalyticsHarness: Send + Sync {
    /// A fresh adapter.
    async fn driver(&self) -> Arc<dyn AnalyticsDriver>;

    /// A query with data, and its value.
    fn known(&self) -> (MetricQuery, f64);

    /// A query of the same metric over a window with no data.
    fn empty(&self) -> MetricQuery;
}

/// Runs every analytics case (spec 3.5 §7.12, §7.13): a known value reads exactly, the
/// metric source reads the same value, a window without data reads nothing (or zero), and a
/// reversed window or an unknown metric is refused before any call.
///
/// # Panics
///
/// When the adapter breaks the contract, naming the case.
pub async fn analytics_driver_contract(harness: &dyn AnalyticsHarness) {
    let driver = harness.driver().await;
    let (query, expected) = harness.known();
    assert!(driver.metrics().contains(&query.metric), "metrics: `{}` is listed", query.metric);
    assert_eq!(
        driver.metric(&query).await.expect("known: reads"),
        Some(expected),
        "known: the value"
    );
    let source = AnalyticsMetrics::new(Arc::clone(&driver));
    let read = source
        .read(Uuid::nil(), &format!("analytics.{}", query.metric), query.from, query.to)
        .await
        .expect("source: reads");
    assert_eq!(read, Some(expected), "source: the same value through the metric source");
    let empty = driver.metric(&harness.empty()).await.expect("empty: reads");
    assert!(
        matches!(empty, None | Some(0.0)),
        "empty: nothing for a window without data: {empty:?}"
    );
    let reversed = MetricQuery::new(query.metric.clone(), query.to, query.from);
    let error = driver.metric(&reversed).await.expect_err("reversed: refused");
    assert!(matches!(error, DriverError::Invalid { .. }), "reversed: invalid: {error:?}");
    let unknown = MetricQuery::new("no_such_metric_zz", query.from, query.to);
    let error = driver.metric(&unknown).await.expect_err("unknown: refused");
    assert!(matches!(error, DriverError::Invalid { .. }), "unknown: invalid: {error:?}");
    assert!(
        source.read(Uuid::nil(), "visitors", query.from, query.to).await.is_err(),
        "source: unprefixed refused"
    );
}

/// Makes fresh search-console adapters for [`search_console_driver_contract`].
#[async_trait]
pub trait SearchConsoleHarness: Send + Sync {
    /// A fresh adapter.
    async fn driver(&self) -> Arc<dyn SearchConsoleDriver>;

    /// A window with data, and its performance.
    fn known(&self) -> (DateTime<Utc>, DateTime<Utc>, SearchPerformance);

    /// A window with no data.
    fn empty(&self) -> (DateTime<Utc>, DateTime<Utc>);
}

/// Runs every search-console case (spec 3.5 §7.12, §7.13): a known window reads its
/// performance, the metric source reads its clicks, a window without data reads nothing,
/// and a reversed window is refused.
///
/// # Panics
///
/// When the adapter breaks the contract, naming the case.
pub async fn search_console_driver_contract(harness: &dyn SearchConsoleHarness) {
    let driver = harness.driver().await;
    let (from, to, expected) = harness.known();
    let read = driver.performance(from, to, &SearchFilter::default()).await.expect("known: reads");
    assert_eq!(read, Some(expected), "known: the performance");
    let source = SearchMetrics::new(Arc::clone(&driver));
    let clicks = source.read(Uuid::nil(), "search.clicks", from, to).await.expect("source: reads");
    assert_eq!(clicks, Some(expected.clicks), "source: clicks through the metric source");
    let (empty_from, empty_to) = harness.empty();
    let empty = driver
        .performance(empty_from, empty_to, &SearchFilter::default())
        .await
        .expect("empty: reads");
    assert_eq!(empty, None, "empty: nothing for a window without data");
    let error = driver
        .performance(to, from, &SearchFilter::default())
        .await
        .expect_err("reversed: refused");
    assert!(matches!(error, DriverError::Invalid { .. }), "reversed: invalid: {error:?}");
}
