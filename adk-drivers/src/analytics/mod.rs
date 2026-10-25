//! Analytics and search-console drivers (spec 3.5 §7.12, §7.13): read-only metrics, each a
//! [`KpiSource`] for the KPI tracker.
//!
//! | Tool | Effect class | Side | Default cap |
//! |---|---|---|---|
//! | `analytics_metric` | `read` | back office | Read |
//! | `search_console_performance` | `read` | back office | Read |
//!
//! | Adapter | Contract | Feature |
//! |---|---|---|
//! | [`InMemoryAnalytics`], [`InMemorySearchConsole`] | [`AnalyticsDriver`], [`SearchConsoleDriver`] | — |
//! | `PlausibleAnalytics`: the Plausible Stats API v2, cookieless and self-hostable | [`AnalyticsDriver`] | `plausible` |
//! | `SearchAnalyticsApi`: a Search Analytics `query` endpoint, as Google Search Console serves it | [`SearchConsoleDriver`] | `search-console` |

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use adk_core::Tool;
use adk_kernel::drivers::EffectClass;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::class::{AutonomyCap, Side, ToolSpec};
use crate::email::time_arg;
use crate::error::{DriverError, Result};
use crate::metrics::{KpiSource, unknown_metric};
use crate::outbound::str_arg;
use crate::tools::{DriverTool, spec};

#[cfg(feature = "plausible")]
mod plausible;
#[cfg(feature = "search-console")]
mod search_console;

#[cfg(feature = "plausible")]
pub use plausible::PlausibleAnalytics;
#[cfg(feature = "search-console")]
pub use search_console::SearchAnalyticsApi;

/// One metric over a window (spec 3.5 §7.12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct MetricQuery {
    /// The metric, such as `visitors`.
    pub metric: String,
    /// The start, inclusive.
    pub from: DateTime<Utc>,
    /// The end, exclusive.
    pub to: DateTime<Utc>,
    /// Filters by dimension, such as `event:page` = `/pricing`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub filters: BTreeMap<String, String>,
}

impl MetricQuery {
    /// `metric` over `[from, to)`, unfiltered.
    pub fn new(metric: impl Into<String>, from: DateTime<Utc>, to: DateTime<Utc>) -> Self {
        Self { metric: metric.into(), from, to, filters: BTreeMap::new() }
    }

    /// Adds a filter.
    #[must_use]
    pub fn filter(mut self, dimension: impl Into<String>, value: impl Into<String>) -> Self {
        self.filters.insert(dimension.into(), value.into());
        self
    }

    /// Refuses an empty or reversed window.
    pub(crate) fn check(&self) -> Result<()> {
        if self.to <= self.from {
            return Err(DriverError::invalid("a metric window ends after it starts"));
        }
        Ok(())
    }
}

/// The days `[from, to)` covers, as the first and last calendar day in UTC.
pub(crate) fn day_range(from: DateTime<Utc>, to: DateTime<Utc>) -> (NaiveDate, NaiveDate) {
    let last = to - chrono::Duration::seconds(1);
    (from.date_naive(), last.date_naive().max(from.date_naive()))
}

/// A site analytics service (spec 3.5 §7.12, §7.13): reads metrics, never writes.
#[async_trait]
pub trait AnalyticsDriver: Send + Sync {
    /// The adapter's name.
    fn name(&self) -> &str;

    /// The metrics it answers.
    fn metrics(&self) -> Vec<String>;

    /// The value of `query`, or `None` when the service has no data for the window.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a metric it does not answer or a reversed
    /// window, and the service's errors.
    async fn metric(&self, query: &MetricQuery) -> Result<Option<f64>>;
}

/// Search performance over a window (spec 3.5 §7.12): SEO from a search console.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SearchPerformance {
    /// Clicks from search results.
    pub clicks: f64,
    /// Times a result was shown.
    pub impressions: f64,
    /// Clicks per impression.
    pub ctr: f64,
    /// The average position, 1 at the top.
    pub position: f64,
}

/// What a search-performance read is narrowed to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct SearchFilter {
    /// One search query.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// One page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

/// A search console (spec 3.5 §7.12, §7.13): reads search performance, never writes.
#[async_trait]
pub trait SearchConsoleDriver: Send + Sync {
    /// The adapter's name.
    fn name(&self) -> &str;

    /// Performance over the days of `[from, to)`, or `None` when there is no data.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a reversed window, and the service's errors.
    async fn performance(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        filter: &SearchFilter,
    ) -> Result<Option<SearchPerformance>>;
}

/// The metrics of an [`AnalyticsDriver`] as a [`KpiSource`] named `analytics`, its metrics
/// prefixed `analytics.`.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_drivers::KpiSource;
/// use adk_drivers::analytics::{AnalyticsMetrics, InMemoryAnalytics};
///
/// # futures::executor::block_on(async {
/// let now = chrono::Utc::now();
/// let analytics = InMemoryAnalytics::new().with("visitors", now, 40.0);
/// let source = AnalyticsMetrics::new(Arc::new(analytics));
/// let value = source.read(uuid::Uuid::nil(), "analytics.visitors", now - chrono::Duration::days(1), now + chrono::Duration::days(1)).await.unwrap();
/// assert_eq!(value, Some(40.0));
/// # });
/// ```
pub struct AnalyticsMetrics {
    driver: Arc<dyn AnalyticsDriver>,
}

impl AnalyticsMetrics {
    /// The source over `driver`.
    pub fn new(driver: Arc<dyn AnalyticsDriver>) -> Self {
        Self { driver }
    }
}

impl AnalyticsMetrics {
    /// The metrics it measures, as KPIs name them.
    pub fn metrics(&self) -> Vec<String> {
        self.driver.metrics().into_iter().map(|metric| format!("analytics.{metric}")).collect()
    }

    /// The value of `metric` over `[from, to)`; `None` when there is no data for the
    /// window.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a metric it does
    /// not measure, and the service's errors.
    pub async fn measure(
        &self,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Option<f64>> {
        let Some(name) = metric.strip_prefix("analytics.") else {
            return Err(unknown_metric("analytics", self.metrics(), metric));
        };
        self.driver.metric(&MetricQuery::new(name, from, to)).await
    }
}

#[async_trait]
impl KpiSource for AnalyticsMetrics {
    fn name(&self) -> &str {
        "analytics"
    }

    async fn read(
        &self,
        _site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        Ok(self.measure(metric, from, to).await?)
    }
}

/// The metrics of a [`SearchConsoleDriver`] as a [`KpiSource`] named `search`:
/// `search.clicks`, `search.impressions`, `search.ctr` and `search.position`.
pub struct SearchMetrics {
    driver: Arc<dyn SearchConsoleDriver>,
}

impl SearchMetrics {
    /// The source over `driver`.
    pub fn new(driver: Arc<dyn SearchConsoleDriver>) -> Self {
        Self { driver }
    }
}

impl SearchMetrics {
    /// The metrics it measures, as KPIs name them.
    pub fn metrics(&self) -> Vec<String> {
        ["search.clicks", "search.impressions", "search.ctr", "search.position"]
            .map(String::from)
            .to_vec()
    }

    /// The value of `metric` over `[from, to)`; `None` when there is no data for the
    /// window.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for a metric it does
    /// not measure, and the service's errors.
    pub async fn measure(
        &self,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Option<f64>> {
        if !self.metrics().iter().any(|known| known == metric) {
            return Err(unknown_metric("search", self.metrics(), metric));
        }
        let Some(performance) = self.driver.performance(from, to, &SearchFilter::default()).await?
        else {
            return Ok(None);
        };
        Ok(Some(match metric {
            "search.clicks" => performance.clicks,
            "search.impressions" => performance.impressions,
            "search.ctr" => performance.ctr,
            _ => performance.position,
        }))
    }
}

#[async_trait]
impl KpiSource for SearchMetrics {
    fn name(&self) -> &str {
        "search"
    }

    async fn read(
        &self,
        _site: Uuid,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> adk_kernel::Result<Option<f64>> {
        Ok(self.measure(metric, from, to).await?)
    }
}

/// Analytics in process memory: daily values per metric, summed over a window, for tests
/// and as the hermetic reference of the [`AnalyticsDriver`] conformance suite.
#[derive(Debug, Default)]
pub struct InMemoryAnalytics {
    days: Mutex<BTreeMap<(String, NaiveDate), f64>>,
}

impl InMemoryAnalytics {
    /// No data.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `value` of `metric` on the day of `at`.
    #[must_use]
    pub fn with(self, metric: &str, at: DateTime<Utc>, value: f64) -> Self {
        self.record(metric, at, value);
        self
    }

    /// Adds `value` to `metric` on the day of `at`.
    pub fn record(&self, metric: &str, at: DateTime<Utc>, value: f64) {
        *self
            .days
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry((metric.to_string(), at.date_naive()))
            .or_default() += value;
    }
}

#[async_trait]
impl AnalyticsDriver for InMemoryAnalytics {
    fn name(&self) -> &str {
        "in-memory"
    }

    fn metrics(&self) -> Vec<String> {
        let days = self.days.lock().unwrap_or_else(PoisonError::into_inner);
        let mut metrics: Vec<String> = days.keys().map(|(metric, _)| metric.clone()).collect();
        metrics.dedup();
        metrics
    }

    async fn metric(&self, query: &MetricQuery) -> Result<Option<f64>> {
        query.check()?;
        if !query.filters.is_empty() {
            return Err(DriverError::unsupported("in-memory analytics", "filters"));
        }
        let (first, last) = day_range(query.from, query.to);
        let days = self.days.lock().unwrap_or_else(PoisonError::into_inner);
        if !days.keys().any(|(metric, _)| *metric == query.metric) {
            return Err(DriverError::invalid(format!(
                "in-memory analytics has no metric `{}`",
                query.metric
            )));
        }
        let values: Vec<f64> = days
            .iter()
            .filter(|((metric, day), _)| *metric == query.metric && *day >= first && *day <= last)
            .map(|(_, value)| *value)
            .collect();
        Ok((!values.is_empty()).then(|| values.iter().sum()))
    }
}

/// A search console in process memory: daily clicks, impressions and position, for tests
/// and as the hermetic reference of the [`SearchConsoleDriver`] conformance suite.
#[derive(Debug, Default)]
pub struct InMemorySearchConsole {
    days: Mutex<BTreeMap<NaiveDate, (f64, f64, f64)>>,
}

impl InMemorySearchConsole {
    /// No data.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a day of `clicks`, `impressions` and average `position`.
    #[must_use]
    pub fn with_day(self, day: NaiveDate, clicks: f64, impressions: f64, position: f64) -> Self {
        self.days
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(day, (clicks, impressions, position));
        self
    }
}

/// Days of `(clicks, impressions, position)` combined: sums, the ratio, and the
/// impression-weighted position.
pub(crate) fn combine(
    days: impl IntoIterator<Item = (f64, f64, f64)>,
) -> Option<SearchPerformance> {
    let (mut clicks, mut impressions, mut weighted, mut any) = (0.0, 0.0, 0.0, false);
    for (day_clicks, day_impressions, position) in days {
        any = true;
        clicks += day_clicks;
        impressions += day_impressions;
        weighted += position * day_impressions;
    }
    any.then(|| SearchPerformance {
        clicks,
        impressions,
        ctr: if impressions > 0.0 { clicks / impressions } else { 0.0 },
        position: if impressions > 0.0 { weighted / impressions } else { 0.0 },
    })
}

#[async_trait]
impl SearchConsoleDriver for InMemorySearchConsole {
    fn name(&self) -> &str {
        "in-memory"
    }

    async fn performance(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        filter: &SearchFilter,
    ) -> Result<Option<SearchPerformance>> {
        if to <= from {
            return Err(DriverError::invalid("a metric window ends after it starts"));
        }
        if filter.query.is_some() || filter.page.is_some() {
            return Err(DriverError::unsupported("in-memory search console", "filters"));
        }
        let (first, last) = day_range(from, to);
        let days = self.days.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(combine(days.range(first..=last).map(|(_, day)| *day)))
    }
}

fn analytics_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "metric": {"type": "string", "description": "Such as visitors, pageviews or bounce_rate"},
            "from": {"type": "string", "format": "date-time"},
            "to": {"type": "string", "format": "date-time"},
            "filters": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Dimension to value, such as {\"event:page\": \"/pricing\"}"}
        },
        "required": ["metric", "from", "to"]
    })
}

fn search_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "from": {"type": "string", "format": "date-time"},
            "to": {"type": "string", "format": "date-time"},
            "query": {"type": "string"},
            "page": {"type": "string"}
        },
        "required": ["from", "to"]
    })
}

/// The analytics class's tools (spec 3.5 §7.13).
pub const ANALYTICS_TOOLS: &[ToolSpec] = &[ToolSpec {
    name: "analytics_metric",
    description: "Read one site metric over a window, such as visitors or pageviews.",
    class: EffectClass::Read,
    also: None,
    side: Side::BackOffice,
    cap: AutonomyCap::Read,
    budget_arg: None,
    schema: analytics_schema,
}];

/// The search-console class's tools (spec 3.5 §7.13).
pub const SEARCH_CONSOLE_TOOLS: &[ToolSpec] = &[ToolSpec {
    name: "search_console_performance",
    description: "Read search clicks, impressions, click-through rate and average position over a window.",
    class: EffectClass::Read,
    also: None,
    side: Side::BackOffice,
    cap: AutonomyCap::Read,
    budget_arg: None,
    schema: search_schema,
}];

/// The analytics tools over `driver`.
pub fn analytics_tools(driver: Arc<dyn AnalyticsDriver>) -> Vec<Arc<dyn Tool>> {
    let name = format!("analytics/{}", driver.name());
    vec![DriverTool::shared(spec(ANALYTICS_TOOLS, "analytics_metric"), &name, move |args| {
        let driver = Arc::clone(&driver);
        Box::pin(async move {
            let mut query = MetricQuery::new(
                str_arg(&args, "metric")?,
                time_arg(&args, "from")?,
                time_arg(&args, "to")?,
            );
            if let Some(filters) = args.get("filters").and_then(Value::as_object) {
                for (dimension, value) in filters {
                    let value = value
                        .as_str()
                        .ok_or_else(|| DriverError::invalid("a filter value is a string"))?;
                    query = query.filter(dimension.as_str(), value);
                }
            }
            let value = driver.metric(&query).await?;
            Ok(
                json!({ "metric": query.metric, "from": query.from, "to": query.to, "value": value }),
            )
        })
    })]
}

/// The search-console tools over `driver`.
pub fn search_console_tools(driver: Arc<dyn SearchConsoleDriver>) -> Vec<Arc<dyn Tool>> {
    let name = format!("search_console/{}", driver.name());
    vec![DriverTool::shared(
        spec(SEARCH_CONSOLE_TOOLS, "search_console_performance"),
        &name,
        move |args| {
            let driver = Arc::clone(&driver);
            Box::pin(async move {
                let filter = SearchFilter {
                    query: args.get("query").and_then(Value::as_str).map(str::to_string),
                    page: args.get("page").and_then(Value::as_str).map(str::to_string),
                };
                let performance = driver
                    .performance(time_arg(&args, "from")?, time_arg(&args, "to")?, &filter)
                    .await?;
                Ok(json!({ "performance": performance }))
            })
        },
    )]
}
