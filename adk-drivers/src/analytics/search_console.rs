//! Search performance through a Search Analytics API (spec 3.5 §7.12, §7.13; feature
//! `search-console`).

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::{SearchConsoleDriver, SearchFilter, SearchPerformance, day_range};
use crate::error::{DriverError, Result};
use crate::http::{checked_base, http_client, read_json};

const SERVICE: &str = "search-console";

/// Google's API root, which serves the Search Console API's Search Analytics endpoint.
pub const GOOGLE_API_BASE: &str = "https://www.googleapis.com/";

/// Search performance through a Search Analytics `query` endpoint (spec 3.5 §7.12, §7.13):
/// `POST {base}webmasters/v3/sites/{site}/searchAnalytics/query`, as Google Search Console
/// serves it, or any service with the same shape.
///
/// The access token comes from the credential broker's OAuth flow (A-I19); the driver only
/// reads.
///
/// # Example
///
/// ```rust,no_run
/// use adk_drivers::analytics::{SearchAnalyticsApi, SearchConsoleDriver, SearchFilter};
///
/// # async fn example(token: String) -> adk_drivers::Result<()> {
/// let console = SearchAnalyticsApi::google("sc-domain:bakery.example", token)?;
/// let now = chrono::Utc::now();
/// let week = console.performance(now - chrono::Duration::days(7), now, &SearchFilter::default()).await?;
/// # let _ = week;
/// # Ok(())
/// # }
/// ```
pub struct SearchAnalyticsApi {
    http: reqwest::Client,
    base: String,
    site: String,
    token: String,
}

impl std::fmt::Debug for SearchAnalyticsApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchAnalyticsApi")
            .field("base", &self.base)
            .field("site", &self.site)
            .finish_non_exhaustive()
    }
}

impl SearchAnalyticsApi {
    /// Property `site` (`https://bakery.example/` or `sc-domain:bakery.example`) on the API
    /// at `base`, read with `token`.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for an address that is not `https` (or `http` on
    /// loopback) or an empty site or token.
    pub fn new(base: &str, site: &str, token: impl Into<String>) -> Result<Self> {
        let token = token.into();
        if site.trim().is_empty() || token.is_empty() {
            return Err(DriverError::invalid(
                "a search console needs the property and an access token",
            ));
        }
        Ok(Self {
            http: http_client(SERVICE, Duration::from_secs(20))?,
            base: checked_base(base)?,
            site: site.trim().to_string(),
            token,
        })
    }

    /// Property `site` on Google's Search Console API.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::Invalid`] for an empty site or token.
    pub fn google(site: &str, token: impl Into<String>) -> Result<Self> {
        Self::new(GOOGLE_API_BASE, site, token)
    }

    fn endpoint(&self) -> Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(&self.base)
            .map_err(|error| DriverError::invalid(error.to_string()))?;
        url.path_segments_mut()
            .map_err(|()| DriverError::invalid("the API address cannot take a path"))?
            .pop_if_empty()
            .extend(["webmasters", "v3", "sites", &self.site, "searchAnalytics", "query"]);
        Ok(url)
    }
}

/// The Search Analytics query over the days of `[from, to)` narrowed by `filter`.
pub(crate) fn query_body(from: DateTime<Utc>, to: DateTime<Utc>, filter: &SearchFilter) -> Value {
    let (first, last) = day_range(from, to);
    let mut filters = Vec::new();
    if let Some(query) = &filter.query {
        filters.push(json!({"dimension": "query", "operator": "equals", "expression": query}));
    }
    if let Some(page) = &filter.page {
        filters.push(json!({"dimension": "page", "operator": "equals", "expression": page}));
    }
    let mut body = json!({
        "startDate": first.to_string(),
        "endDate": last.to_string(),
        "dimensions": [],
        "rowLimit": 1,
    });
    if !filters.is_empty() {
        body["dimensionFilterGroups"] = json!([{ "filters": filters }]);
    }
    body
}

#[async_trait]
impl SearchConsoleDriver for SearchAnalyticsApi {
    fn name(&self) -> &str {
        SERVICE
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
        let response = self
            .http
            .post(self.endpoint()?)
            .bearer_auth(&self.token)
            .json(&query_body(from, to, filter))
            .send()
            .await
            .map_err(|error| DriverError::unavailable(SERVICE, error))?;
        let answer = read_json(SERVICE, response).await?;
        let Some(row) = answer.get("rows").and_then(Value::as_array).and_then(|rows| rows.first())
        else {
            return Ok(None);
        };
        let number = |name: &str| {
            row.get(name)
                .and_then(Value::as_f64)
                .ok_or_else(|| DriverError::protocol(SERVICE, format!("a row has no `{name}`")))
        };
        Ok(Some(SearchPerformance {
            clicks: number("clicks")?,
            impressions: number("impressions")?,
            ctr: number("ctr")?,
            position: number("position")?,
        }))
    }
}
