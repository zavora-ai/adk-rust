//! The reference adapters over web APIs against servers on loopback that answer with
//! recorded responses (spec 3.5 §7.13, §11): RDAP, the Plausible Stats API, a Search
//! Analytics API and the Mastodon client API.

#![cfg(all(
    feature = "rdap",
    feature = "plausible",
    feature = "search-console",
    feature = "mastodon"
))]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use adk_drivers::analytics::{
    AnalyticsDriver, MetricQuery, PlausibleAnalytics, SearchAnalyticsApi, SearchConsoleDriver,
    SearchPerformance,
};
use adk_drivers::conformance::{
    AnalyticsHarness, Published, RegistrarHarness, SearchConsoleHarness, SocialHarness,
    SocialServer, analytics_driver_contract, registrar_driver_contract,
    search_console_driver_contract, social_driver_contract,
};
use adk_drivers::dns::{Money, RdapRegistrar, RegistrarDriver};
use adk_drivers::social::{MastodonSocial, SocialDriver, SocialItem};
use async_trait::async_trait;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

const TOKEN: &str = "test-token-123";

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// A server on loopback, stopped when dropped.
struct Served {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Served {
    async fn start(app: Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { addr, task }
    }

    fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }
}

fn authorized(headers: &HeaderMap) -> bool {
    headers.get("authorization").and_then(|value| value.to_str().ok())
        == Some(&format!("Bearer {TOKEN}"))
}

// RDAP.

async fn rdap_domain(Path(domain): Path<String>) -> Response {
    if domain.eq_ignore_ascii_case("bakery.example") {
        Json(fixture("rdap_domain.json")).into_response()
    } else {
        (StatusCode::NOT_FOUND, Json(json!({"errorCode": 404, "title": "Not Found"})))
            .into_response()
    }
}

struct Rdap {
    server: Served,
}

#[async_trait]
impl RegistrarHarness for Rdap {
    fn domain(&self) -> String {
        "bakery.example".into()
    }

    fn price_per_year(&self) -> Money {
        Money::new(1_400, "USD")
    }

    async fn driver(&self) -> Arc<dyn RegistrarDriver> {
        Arc::new(
            RdapRegistrar::new()
                .unwrap()
                .with_base(&format!("{}rdap/", self.server.url()))
                .unwrap(),
        )
    }
}

#[tokio::test]
async fn rdap_keeps_the_registrar_contract() {
    let server =
        Served::start(Router::new().route("/rdap/domain/{domain}", get(rdap_domain))).await;
    let harness = Rdap { server };
    registrar_driver_contract(&harness).await;
    let status = harness.driver().await.domain("Bakery.Example.").await.unwrap();
    assert_eq!(status.expires_at, Some(Utc.with_ymd_and_hms(2026, 11, 20, 9, 15, 0).unwrap()));
    assert_eq!(status.registrar.as_deref(), Some("Example Registrar, Inc."));
    assert!(status.statuses.contains(&"client transfer prohibited".to_string()));
}

#[test]
fn an_rdap_server_in_clear_text_off_loopback_is_refused() {
    assert!(RdapRegistrar::new().unwrap().with_base("http://rdap.example/").is_err());
}

// Plausible.

type Requests = Arc<Mutex<Vec<Value>>>;

async fn plausible_query(
    State(requests): State<Requests>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Invalid API key"})))
            .into_response();
    }
    requests.lock().unwrap_or_else(PoisonError::into_inner).push(body.clone());
    let range = body["date_range"][0].as_str().unwrap_or_default();
    if body["site_id"] != "bakery.example" {
        return (StatusCode::BAD_REQUEST, Json(json!({"error": "unknown site"}))).into_response();
    }
    if range.starts_with("2026-10-05") {
        Json(fixture("plausible_visitors.json")).into_response()
    } else {
        Json(fixture("plausible_empty.json")).into_response()
    }
}

fn at(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, day, 0, 0, 0).unwrap()
}

struct Plausible {
    server: Served,
}

#[async_trait]
impl AnalyticsHarness for Plausible {
    async fn driver(&self) -> Arc<dyn AnalyticsDriver> {
        Arc::new(PlausibleAnalytics::new(&self.server.url(), "bakery.example", TOKEN).unwrap())
    }

    fn known(&self) -> (MetricQuery, f64) {
        (MetricQuery::new("visitors", at(5), at(7)), 200.0)
    }

    fn empty(&self) -> MetricQuery {
        MetricQuery::new("visitors", at(20), at(21))
    }
}

#[tokio::test]
async fn plausible_keeps_the_analytics_contract() {
    let requests: Requests = Arc::default();
    let app = Router::new()
        .route("/api/v2/query", post(plausible_query))
        .with_state(Arc::clone(&requests));
    let harness = Plausible { server: Served::start(app).await };
    analytics_driver_contract(&harness).await;
    let first = requests.lock().unwrap()[0].clone();
    assert_eq!(
        first,
        json!({
            "site_id": "bakery.example",
            "metrics": ["visitors"],
            "date_range": ["2026-10-05T00:00:00+00:00", "2026-10-06T23:59:59+00:00"],
        }),
        "the Stats API v2 query"
    );
    let filtered = MetricQuery::new("pageviews", at(5), at(7)).filter("event:page", "/pricing");
    harness.driver().await.metric(&filtered).await.unwrap();
    let last = requests.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        last["filters"],
        json!([["is", "event:page", ["/pricing"]]]),
        "filters as Plausible writes them"
    );
    let wrong = PlausibleAnalytics::new(&harness.server.url(), "bakery.example", "wrong").unwrap();
    let error = wrong.metric(&MetricQuery::new("visitors", at(5), at(7))).await.unwrap_err();
    assert!(matches!(error, adk_drivers::DriverError::Unauthorized { .. }), "{error:?}");
}

// Search Analytics.

async fn search_query(
    Path(site): Path<String>,
    State(requests): State<Requests>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": {"code": 401}}))).into_response();
    }
    requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(json!({"site": site, "body": body}));
    if site != "sc-domain:bakery.example" {
        return (StatusCode::FORBIDDEN, Json(json!({"error": {"code": 403}}))).into_response();
    }
    if body["startDate"] == "2026-10-05" && body["endDate"] == "2026-10-06" {
        Json(fixture("search_analytics.json")).into_response()
    } else {
        Json(fixture("search_analytics_empty.json")).into_response()
    }
}

struct Search {
    server: Served,
}

#[async_trait]
impl SearchConsoleHarness for Search {
    async fn driver(&self) -> Arc<dyn SearchConsoleDriver> {
        Arc::new(
            SearchAnalyticsApi::new(&self.server.url(), "sc-domain:bakery.example", TOKEN).unwrap(),
        )
    }

    fn known(&self) -> (DateTime<Utc>, DateTime<Utc>, SearchPerformance) {
        let performance = serde_json::from_value(
            json!({"clicks": 40.0, "impressions": 400.0, "ctr": 0.1, "position": 7.0}),
        );
        (at(5), at(7), performance.unwrap())
    }

    fn empty(&self) -> (DateTime<Utc>, DateTime<Utc>) {
        (at(20), at(21))
    }
}

#[tokio::test]
async fn a_search_analytics_api_keeps_the_search_console_contract() {
    let requests: Requests = Arc::default();
    let app = Router::new()
        .route("/webmasters/v3/sites/{site}/searchAnalytics/query", post(search_query))
        .with_state(Arc::clone(&requests));
    let harness = Search { server: Served::start(app).await };
    search_console_driver_contract(&harness).await;
    let first = requests.lock().unwrap()[0].clone();
    assert_eq!(
        first["body"],
        json!({"startDate": "2026-10-05", "endDate": "2026-10-06", "dimensions": [], "rowLimit": 1}),
        "the Search Analytics query"
    );
}

// Mastodon.

#[derive(Default)]
struct Mastodon {
    notifications: Vec<Value>,
    posted: Vec<Value>,
    next: u64,
}

type MastodonState = Arc<Mutex<Mastodon>>;

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn notifications(
    State(state): State<MastodonState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !authorized(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "The access token is invalid"})))
            .into_response();
    }
    assert_eq!(
        query.get("types[]").map(String::as_str),
        Some("mention"),
        "only mentions are asked for"
    );
    let limit: usize = query.get("limit").and_then(|limit| limit.parse().ok()).unwrap_or(15);
    let after: u64 = query.get("min_id").and_then(|id| id.parse().ok()).unwrap_or(0);
    let state = state.lock().unwrap_or_else(PoisonError::into_inner);
    let mut page: Vec<Value> = state
        .notifications
        .iter()
        .filter(|notification| {
            notification["id"].as_str().and_then(|id| id.parse::<u64>().ok()).unwrap_or(0) > after
        })
        .take(limit)
        .cloned()
        .collect();
    page.reverse();
    Json(Value::Array(page)).into_response()
}

async fn statuses(
    State(state): State<MastodonState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if !authorized(&headers) {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "The access token is invalid"})))
            .into_response();
    }
    assert!(headers.contains_key("idempotency-key"), "every status carries an Idempotency-Key");
    let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
    state.next += 1;
    let id = format!("9{:04}", state.next);
    state.posted.push(body.clone());
    let mut status = fixture("mastodon_status.json");
    status["id"] = json!(id);
    status["url"] = json!(format!("https://social.example/@site/{id}"));
    status["content"] =
        json!(format!("<p>{}</p>", escape(body["status"].as_str().unwrap_or_default())));
    status["visibility"] = body["visibility"].clone();
    status["in_reply_to_id"] = body["in_reply_to_id"].clone();
    Json(status).into_response()
}

/// The fake's state, and the server kept alive with it.
struct MastodonServer {
    state: MastodonState,
    _served: Arc<Served>,
}

#[async_trait]
impl SocialServer for MastodonServer {
    async fn receive(&self, item: &SocialItem) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.next += 1;
        let mut status = fixture("mastodon_status.json");
        status["id"] = json!(item.id);
        status["visibility"] = json!(item.visibility);
        status["content"] = json!(format!("<p>{}</p>", escape(&item.text)));
        status["account"]["acct"] = json!(item.author);
        let notification = json!({"id": format!("{}", 1000 + state.next), "type": "mention", "created_at": status["created_at"], "status": status});
        state.notifications.push(notification);
    }

    async fn published(&self) -> Vec<Published> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .posted
            .iter()
            .map(|body| Published {
                in_reply_to: body["in_reply_to_id"].as_str().map(str::to_string),
                text: body["status"].as_str().unwrap_or_default().to_string(),
                visibility: body["visibility"].as_str().unwrap_or_default().to_string(),
            })
            .collect()
    }
}

struct MastodonHarness;

#[async_trait]
impl SocialHarness for MastodonHarness {
    async fn start(&self) -> (Arc<dyn SocialDriver>, Arc<dyn SocialServer>) {
        let state: MastodonState = Arc::default();
        let app = Router::new()
            .route("/api/v1/notifications", get(notifications))
            .route("/api/v1/statuses", post(statuses))
            .with_state(Arc::clone(&state));
        let served = Arc::new(Served::start(app).await);
        let social = MastodonSocial::new(&served.url(), TOKEN).unwrap();
        (Arc::new(social), Arc::new(MastodonServer { state, _served: served }))
    }
}

#[tokio::test]
async fn mastodon_keeps_the_social_contract() {
    social_driver_contract(&MastodonHarness).await;
    let (social, _server) = MastodonHarness.start().await;
    assert_eq!(social.max_chars(), 500);
    let wrong = MastodonSocial::new("http://127.0.0.1:9/", "x").unwrap();
    assert!(wrong.mentions(None, 5).await.is_err(), "an unreachable server is an error");
}
