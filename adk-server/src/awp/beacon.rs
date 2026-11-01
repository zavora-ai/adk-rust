//! `POST /awp/v2/beacon`: cookieless measurement (spec 3.5 §7.12; AWP 2.0 §20).
//!
//! A page sends `{"kind": "pageview", "path": "/pricing"}` or `{"kind": "event", "name":
//! "signup"}`. The edge counts it per day and answers `204`:
//!
//! | Rule | Behaviour |
//! |---|---|
//! | No cookies | The beacon sets none and reads none |
//! | No stored addresses | Counts carry no address; the edge's rate limiter keys on the address in memory only |
//! | No identifiers without a receipt | A visitor is counted once a day only under a live `persistent_profile` receipt the beacon names, by its pseudonymous `subjectRef` |
//! | Global Privacy Control | A request with `Sec-GPC: 1` counts nothing |
//!
//! Query strings and fragments are cut from paths, so a URL never carries personal data into a
//! count. [`BeaconCounts`] reads the counts for the KPI tracker.
//!
//! | Metric | Counts |
//! |---|---|
//! | `pageviews` | Page views |
//! | `pageviews:{path}` | Page views of one path |
//! | `events:{name}` | One event |
//! | `visitors` | Distinct consenting visitors, summed per day |

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use adk_awp::v2::store::{ConsentReceiptStore, StoreError, StoreResult};
use adk_core::subject::{StoreErasure, SubjectDataStore, SubjectScope, unsupported_error};
use async_trait::async_trait;
use awp_types::{ConsentScope, ErrorBody, ErrorCode};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use uuid::Uuid;

/// The beacon's path.
pub const BEACON_PATH: &str = "/awp/v2/beacon";
/// The largest beacon body.
pub const MAX_BEACON_BODY: usize = 2 * 1024;

/// Daily counts behind the beacon (spec 3.5 §7.12).
#[async_trait]
pub trait BeaconStore: Send + Sync {
    /// Adds `count` to `metric` on `day`.
    async fn add(&self, day: NaiveDate, metric: &str, count: u64) -> StoreResult<()>;

    /// Records `visitor`, a pseudonymous subject reference, as seen on `day`.
    async fn add_visitor(&self, day: NaiveDate, visitor: &str) -> StoreResult<()>;

    /// The total of `metric` over the days in `[from, to)`; `visitors` sums distinct
    /// visitors per day.
    async fn total(&self, metric: &str, from: NaiveDate, to: NaiveDate) -> StoreResult<u64>;

    /// Replaces `visitor` in every day's visitor set with a random token, so the day's count
    /// stays and no set names the visitor; returns how many days named it.
    async fn forget_visitor(&self, visitor: &str) -> StoreResult<u64>;
}

/// The token that takes an erased visitor's place in a day's visitor set.
pub(crate) fn forgotten() -> String {
    format!("erased:{}", Uuid::new_v4().simple())
}

/// The beacon's visitor sets as a [`SubjectDataStore`]: erasure replaces the subject in every
/// day's set (counts stay); export is unsupported, since the sets record only presence.
pub struct BeaconSubjectData {
    store: Arc<dyn BeaconStore>,
}

impl BeaconSubjectData {
    /// The visitor sets in `store`.
    pub fn new(store: Arc<dyn BeaconStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl SubjectDataStore for BeaconSubjectData {
    fn name(&self) -> &str {
        "awp.beacon"
    }

    async fn export(&self, _scope: &SubjectScope) -> adk_core::Result<serde_json::Value> {
        Err(unsupported_error("awp.beacon", "export"))
    }

    async fn erase(&self, scope: &SubjectScope) -> adk_core::Result<StoreErasure> {
        let mut forgotten = 0;
        for subject in scope.subjects() {
            forgotten += self
                .store
                .forget_visitor(&subject.id)
                .await
                .map_err(|error| adk_core::AdkError::session(error.to_string()))?;
        }
        Ok(StoreErasure::deleted(forgotten))
    }
}

/// An in-memory [`BeaconStore`].
#[derive(Debug, Clone, Default)]
pub struct InMemoryBeaconStore {
    counts: Arc<Mutex<BTreeMap<(NaiveDate, String), u64>>>,
    visitors: Arc<Mutex<BTreeMap<NaiveDate, BTreeSet<String>>>>,
}

impl InMemoryBeaconStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl BeaconStore for InMemoryBeaconStore {
    async fn add(&self, day: NaiveDate, metric: &str, count: u64) -> StoreResult<()> {
        *self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry((day, metric.to_owned()))
            .or_default() += count;
        Ok(())
    }

    async fn add_visitor(&self, day: NaiveDate, visitor: &str) -> StoreResult<()> {
        self.visitors
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(day)
            .or_default()
            .insert(visitor.to_owned());
        Ok(())
    }

    async fn forget_visitor(&self, visitor: &str) -> StoreResult<u64> {
        let mut visitors = self.visitors.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut forgot = 0;
        for seen in visitors.values_mut() {
            if seen.remove(visitor) {
                seen.insert(forgotten());
                forgot += 1;
            }
        }
        Ok(forgot)
    }

    async fn total(&self, metric: &str, from: NaiveDate, to: NaiveDate) -> StoreResult<u64> {
        if metric == "visitors" {
            let visitors = self.visitors.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            return Ok(visitors.range(from..to).map(|(_, seen)| seen.len() as u64).sum());
        }
        let counts = self.counts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(counts
            .iter()
            .filter(|((day, name), _)| *day >= from && *day < to && name == metric)
            .map(|(_, count)| *count)
            .sum())
    }
}

/// Reads the beacon's counts for the KPI tracker (spec 3.5 §7.11, §7.12): the shape of the
/// kernel's `KpiSource::read`, so a `beacon` source wraps it directly.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_server::awp::beacon::{BeaconCounts, BeaconStore, InMemoryBeaconStore};
/// use chrono::{TimeZone, Utc};
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let store = Arc::new(InMemoryBeaconStore::new());
/// let day = Utc.with_ymd_and_hms(2026, 10, 25, 0, 0, 0).unwrap();
/// store.add(day.date_naive(), "pageviews", 3).await.unwrap();
/// let counts = BeaconCounts::new(store);
/// let views = counts.read("pageviews", day, day + chrono::Duration::days(1)).await.unwrap();
/// assert_eq!(views, Some(3.0));
/// # }
/// ```
#[derive(Clone)]
pub struct BeaconCounts {
    store: Arc<dyn BeaconStore>,
}

impl std::fmt::Debug for BeaconCounts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BeaconCounts").finish_non_exhaustive()
    }
}

impl BeaconCounts {
    /// The counts in `store`.
    pub fn new(store: Arc<dyn BeaconStore>) -> Self {
        Self { store }
    }

    /// The total of `metric` over the days `[from, to)` cover; `None` for a window with no
    /// whole day.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the store cannot be read.
    pub async fn read(
        &self,
        metric: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Option<f64>, StoreError> {
        let (from, to) = (from.date_naive(), to.date_naive());
        if to <= from {
            return Ok(None);
        }
        Ok(Some(self.store.total(metric, from, to).await? as f64))
    }
}

#[derive(Clone)]
pub(crate) struct BeaconState {
    pub(crate) store: Arc<dyn BeaconStore>,
    pub(crate) consent: Arc<dyn ConsentReceiptStore>,
}

impl BeaconState {
    pub(crate) fn router(&self) -> Router {
        Router::new().route(BEACON_PATH, post(beacon)).with_state(self.clone())
    }
}

#[derive(Deserialize)]
struct Hit {
    kind: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    receipt: Option<Uuid>,
}

fn no_content() -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn invalid(message: &str) -> Response {
    super::envelope(ErrorBody::new(ErrorCode::InvalidRequest, message), None)
}

/// A path without its query string or fragment, at most 256 characters.
fn clean_path(path: &str) -> Option<String> {
    let path = path.split(['?', '#']).next().unwrap_or_default();
    (path.starts_with('/') && path.len() <= 256 && !path.chars().any(char::is_control))
        .then(|| path.to_owned())
}

fn clean_name(name: &str) -> Option<String> {
    (!name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)))
    .then(|| name.to_owned())
}

async fn beacon(State(state): State<BeaconState>, headers: HeaderMap, body: Bytes) -> Response {
    // Global Privacy Control: count nothing.
    if headers.get("sec-gpc").and_then(|value| value.to_str().ok()).map(str::trim) == Some("1") {
        return no_content();
    }
    if body.len() > MAX_BEACON_BODY {
        return invalid("the beacon is too large");
    }
    // `navigator.sendBeacon` posts text/plain, so the body is read as JSON whatever its type.
    let Ok(hit) = serde_json::from_slice::<Hit>(&body) else {
        return invalid("the beacon must be JSON");
    };
    let day = Utc::now().date_naive();
    let mut metrics = Vec::new();
    match hit.kind.as_str() {
        "pageview" => {
            let Some(path) = hit.path.as_deref().and_then(clean_path) else {
                return invalid("a page view names its path");
            };
            metrics.push("pageviews".to_owned());
            metrics.push(format!("pageviews:{path}"));
        }
        "event" => {
            let Some(name) = hit.name.as_deref().and_then(clean_name) else {
                return invalid(
                    "an event names itself in lowercase letters, digits, '.', '_' or '-'",
                );
            };
            metrics.push(format!("events:{name}"));
        }
        _ => return invalid("a beacon is a pageview or an event"),
    }
    for metric in &metrics {
        if let Err(error) = state.store.add(day, metric, 1).await {
            tracing::warn!(error = %error, "awp beacon count not recorded");
        }
    }
    // A visitor is identified only under a live persistent_profile receipt (A-I8).
    if let Some(id) = hit.receipt
        && let Ok(Some(record)) = state.consent.get(id).await
        && record.receipt.scope == ConsentScope::PersistentProfile
        && record.is_live(
            &awp_types::ConsentRequirement {
                scope: ConsentScope::PersistentProfile,
                policy_version: record.receipt.policy_version.clone(),
            },
            Utc::now(),
        )
        && let Err(error) = state.store.add_visitor(day, &record.receipt.subject_ref).await
    {
        tracing::warn!(error = %error, "awp beacon visitor not recorded");
    }
    no_content()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_lose_their_queries_and_names_are_tokens() {
        assert_eq!(clean_path("/pricing?email=a@b.example#top").as_deref(), Some("/pricing"));
        assert_eq!(clean_path("pricing"), None);
        assert_eq!(clean_name("signup.completed").as_deref(), Some("signup.completed"));
        assert_eq!(clean_name("Sign Up"), None);
    }
}
