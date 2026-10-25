//! Analytics through the Plausible Stats API v2 (spec 3.5 §7.12, §7.13; feature
//! `plausible`).

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::{AnalyticsDriver, MetricQuery};
use crate::error::{DriverError, Result};
use crate::http::{checked_base, http_client, read_json};

const SERVICE: &str = "plausible";

/// The metrics the Stats API v2 answers for a site.
pub const PLAUSIBLE_METRICS: [&str; 8] = [
    "visitors",
    "visits",
    "pageviews",
    "views_per_visit",
    "bounce_rate",
    "visit_duration",
    "events",
    "conversion_rate",
];

/// Site analytics through the Plausible Stats API v2 (spec 3.5 §7.12, §7.13): cookieless,
/// open source, hosted or self-hosted, the privacy-respecting default of §13 Q4's external
/// analytics.
///
/// The API key comes from the credential broker and is sent only over `https` (or `http`
/// on loopback). A filter's key is a Plausible dimension such as `event:page` or
/// `visit:source`, matched with `is`.
///
/// # Example
///
/// ```rust,no_run
/// use adk_drivers::analytics::{AnalyticsDriver, MetricQuery, PlausibleAnalytics};
///
/// # async fn example(key: String) -> adk_drivers::Result<()> {
/// let plausible = PlausibleAnalytics::new("https://plausible.io", "bakery.example", key)?;
/// let now = chrono::Utc::now();
/// let visitors = plausible.metric(&MetricQuery::new("visitors", now - chrono::Duration::days(7), now)).await?;
/// # let _ = visitors;
/// # Ok(())
/// # }
/// ```
pub struct PlausibleAnalytics {
    http: reqwest::Client,
    base: String,
    site: String,
    key: String,
}

impl std::fmt::Debug for PlausibleAnalytics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlausibleAnalytics")
            .field("base", &self.base)
            .field("site", &self.site)
            .finish_non_exhaustive()
    }
}

impl PlausibleAnalytics {
    /// The site `site` (its domain as Plausible knows it) on the Plausible instance at
    /// `base`, read with API key `key`.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for an address that is not `https` (or `http` on
    /// loopback) or an empty site or key.
    pub fn new(base: &str, site: &str, key: impl Into<String>) -> Result<Self> {
        let key = key.into();
        if site.trim().is_empty() || key.is_empty() {
            return Err(DriverError::invalid("Plausible needs the site's domain and an API key"));
        }
        Ok(Self {
            http: http_client(SERVICE, Duration::from_secs(20))?,
            base: checked_base(base)?,
            site: site.trim().to_string(),
            key,
        })
    }
}

/// `at` as the Stats API reads a time.
fn timestamp(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
}

/// The Stats API v2 query of `query` for `site`.
pub(crate) fn query_body(site: &str, query: &MetricQuery) -> Value {
    let filters: Vec<Value> =
        query.filters.iter().map(|(dimension, value)| json!(["is", dimension, [value]])).collect();
    let mut body = json!({
        "site_id": site,
        "metrics": [query.metric],
        "date_range": [timestamp(query.from), timestamp(query.to - chrono::Duration::seconds(1))],
    });
    if !filters.is_empty() {
        body["filters"] = Value::Array(filters);
    }
    body
}

#[async_trait]
impl AnalyticsDriver for PlausibleAnalytics {
    fn name(&self) -> &str {
        SERVICE
    }

    fn metrics(&self) -> Vec<String> {
        PLAUSIBLE_METRICS.map(String::from).to_vec()
    }

    async fn metric(&self, query: &MetricQuery) -> Result<Option<f64>> {
        query.check()?;
        if !PLAUSIBLE_METRICS.contains(&query.metric.as_str()) {
            return Err(DriverError::invalid(format!(
                "Plausible does not measure `{}`; use one of {}",
                query.metric,
                PLAUSIBLE_METRICS.join(", ")
            )));
        }
        let response = self
            .http
            .post(format!("{}api/v2/query", self.base))
            .bearer_auth(&self.key)
            .json(&query_body(&self.site, query))
            .send()
            .await
            .map_err(|error| DriverError::unavailable(SERVICE, error))?;
        let answer = read_json(SERVICE, response).await?;
        let results = answer
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| DriverError::protocol(SERVICE, "the answer has no results"))?;
        Ok(results.first().and_then(|row| row.get("metrics")?.get(0)?.as_f64()))
    }
}
