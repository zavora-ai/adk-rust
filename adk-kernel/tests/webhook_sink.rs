//! The outbound webhook sink on the event bus (spec 3.2 §7.6): delivery signed as the
//! webhook ingress verifies it, event selection, bounded retry, dead letters, a slow
//! receiver that never holds up the bus, and logs that never carry the secret or the URL's
//! path.

#![cfg(feature = "webhooks")]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use adk_core::webhook::{WebhookHeaders, WebhookSecret, WebhookVerifier};
use adk_core::webhook_delivery::{InMemoryDeadLetterSink, RetryPolicy};
use adk_kernel::{
    EventBus, EventFilter, EventKind, EventRecord, EventSink, KernelEvent, KernelHealth,
    WebhookSink,
};
use async_trait::async_trait;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

const SECRET: &str = "whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw";

fn secret() -> WebhookSecret {
    WebhookSecret::from_base64(SECRET).unwrap()
}

fn governance(frozen: bool) -> KernelEvent {
    KernelEvent::Governance { frozen, reason: Some("incident 12".into()) }
}

fn wake() -> KernelEvent {
    KernelEvent::Wake { pid: None, source: "cron:nightly".into(), key: "k-1".into(), outcome: None }
}

fn health() -> KernelEvent {
    KernelEvent::Health(KernelHealth::default())
}

/// Retries quickly, so a test waits milliseconds rather than seconds.
fn fast(attempts: u32) -> RetryPolicy {
    RetryPolicy::default()
        .with_max_attempts(attempts)
        .with_initial_backoff(Duration::from_millis(10))
        .with_max_backoff(Duration::from_millis(40))
        .with_attempt_timeout(Duration::from_secs(5))
}

/// One request the receiver accepted.
#[derive(Debug, Clone)]
struct Delivery {
    headers: HeaderMap,
    body: Bytes,
}

impl Delivery {
    fn id(&self) -> String {
        self.headers["webhook-id"].to_str().unwrap().to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    /// Verifies the signature as the webhook ingress does, with `secret`.
    fn verify(&self, secret: WebhookSecret) -> Result<String, String> {
        let headers = WebhookHeaders::from_lookup(|name| {
            self.headers.get(name).and_then(|value| value.to_str().ok())
        })
        .map_err(|error| error.to_string())?;
        WebhookVerifier::new(secret)
            .verify(&headers, &self.body)
            .map(|verified| verified.id)
            .map_err(|error| error.to_string())
    }
}

/// A receiver on loopback: answers each request with the next queued status (`200` when
/// none is queued), after a permit of its gate when it has one.
#[derive(Clone, Default)]
struct Receiver {
    seen: Arc<Mutex<Vec<Delivery>>>,
    statuses: Arc<Mutex<VecDeque<u16>>>,
    gate: Option<Arc<Semaphore>>,
}

impl Receiver {
    fn answering(statuses: impl IntoIterator<Item = u16>) -> Self {
        Self { statuses: Arc::new(Mutex::new(statuses.into_iter().collect())), ..Self::default() }
    }

    fn gated() -> Self {
        Self { gate: Some(Arc::new(Semaphore::new(0))), ..Self::default() }
    }

    fn seen(&self) -> Vec<Delivery> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Serves on a free loopback port and returns the hook's URL.
    async fn serve(&self) -> String {
        let app = Router::new().route("/hooks/kernel", post(receive)).with_state(self.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/hooks/kernel")
    }
}

async fn receive(State(receiver): State<Receiver>, headers: HeaderMap, body: Bytes) -> StatusCode {
    if let Some(gate) = &receiver.gate {
        gate.acquire().await.unwrap().forget();
    }
    receiver.seen.lock().unwrap_or_else(PoisonError::into_inner).push(Delivery { headers, body });
    let status = receiver.statuses.lock().unwrap_or_else(PoisonError::into_inner).pop_front();
    StatusCode::from_u16(status.unwrap_or(200)).unwrap()
}

/// Waits up to ten seconds for `probe`.
async fn eventually(what: &str, mut probe: impl FnMut() -> bool) {
    let started = Instant::now();
    while !probe() {
        assert!(started.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_selected_event_is_delivered_signed_as_the_ingress_verifies_it() {
    let receiver = Receiver::default();
    let url = receiver.serve().await;
    let bus = EventBus::new(16);
    bus.add_sink(Arc::new(WebhookSink::new(&url, secret()).unwrap()));

    let record = bus.publish(governance(true));
    eventually("the delivery", || receiver.seen().len() == 1).await;
    let delivery = &receiver.seen()[0];

    assert_eq!(delivery.verify(secret()), Ok(delivery.id()));
    assert!(delivery.id().starts_with("evt_"), "{}", delivery.id());
    assert_eq!(delivery.headers["content-type"], "application/json");
    assert_eq!(
        delivery.json(),
        json!({
            "type": "governance",
            "timestamp": serde_json::to_value(record.at).unwrap(),
            "data": serde_json::to_value(&record).unwrap(),
        })
    );
    assert_eq!(delivery.json()["data"]["cursor"], record.cursor);
    assert_eq!(
        delivery.json()["data"]["event"],
        json!({ "type": "governance", "frozen": true, "reason": "incident 12" })
    );

    // Another secret, or a changed body, does not verify.
    let other = WebhookSecret::from_bytes(b"another key entirely".to_vec()).unwrap();
    assert!(delivery.verify(other).is_err());
    let tampered =
        Delivery { body: Bytes::from_static(b"{\"type\":\"governance\"}"), ..delivery.clone() };
    assert!(tampered.verify(secret()).is_err());
}

#[tokio::test]
async fn the_filter_selects_events_and_health_is_left_out_by_default() {
    let selected = Receiver::default();
    let defaults = Receiver::default();
    let bus = EventBus::new(16);
    bus.add_sink(Arc::new(
        WebhookSink::new(&selected.serve().await, secret())
            .unwrap()
            .with_filter(EventFilter::only([EventKind::Wake, EventKind::Governance])),
    ));
    bus.add_sink(Arc::new(WebhookSink::new(&defaults.serve().await, secret()).unwrap()));

    bus.publish(health());
    bus.publish(KernelEvent::RuleHit { pid: "run-1".into(), rule_id: "r-1".into() });
    bus.publish(wake());
    bus.publish(governance(true));
    // A marker both sinks select, published last: once it arrives, every earlier event has
    // been delivered or passed over, as each sink delivers in order.
    let marker = bus.publish(governance(false));
    let delivered_marker = |receiver: &Receiver| {
        receiver
            .seen()
            .last()
            .is_some_and(|delivery| delivery.json()["data"]["cursor"] == marker.cursor)
    };
    eventually("the marker", || delivered_marker(&selected) && delivered_marker(&defaults)).await;

    let types = |receiver: &Receiver| -> Vec<String> {
        receiver
            .seen()
            .iter()
            .map(|delivery| delivery.json()["type"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(types(&selected), ["wake", "governance", "governance"]);
    assert_eq!(types(&defaults), ["ruleHit", "wake", "governance", "governance"]);
}

#[tokio::test]
async fn a_failed_delivery_is_retried_under_the_same_id() {
    let receiver = Receiver::answering([503, 429]);
    let dead_letters = Arc::new(InMemoryDeadLetterSink::new());
    let sink = WebhookSink::new(&receiver.serve().await, secret())
        .unwrap()
        .with_retry_policy(fast(5))
        .with_dead_letter_sink(dead_letters.clone());
    let bus = EventBus::new(16);
    bus.add_sink(Arc::new(sink));

    bus.publish(governance(true));
    eventually("three attempts", || receiver.seen().len() == 3).await;
    let seen = receiver.seen();
    for attempt in &seen {
        assert_eq!(attempt.id(), seen[0].id(), "one webhook-id across retries");
        assert_eq!(attempt.verify(secret()), Ok(seen[0].id()), "each attempt is signed");
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(receiver.seen().len(), 3, "a 200 ends the retries");
    assert!(dead_letters.letters().is_empty());
}

#[tokio::test]
async fn an_undeliverable_event_ends_at_the_dead_letter_sink_after_bounded_attempts() {
    let receiver = Receiver::answering([500, 500, 500, 400]);
    let dead_letters = Arc::new(InMemoryDeadLetterSink::new());
    let sink = WebhookSink::new(&receiver.serve().await, secret())
        .unwrap()
        .with_retry_policy(fast(3))
        .with_dead_letter_sink(dead_letters.clone());
    let bus = EventBus::new(16);
    bus.add_sink(Arc::new(sink));

    let retried = bus.publish(governance(true));
    let refused = bus.publish(wake());
    eventually("two dead letters", || dead_letters.letters().len() == 2).await;
    let letters = dead_letters.letters();

    // Three `500`s spend the three attempts; a `400` is not retried.
    assert_eq!((letters[0].attempts, letters[0].last_status), (3, Some(500)));
    assert_eq!((letters[1].attempts, letters[1].last_status), (1, Some(400)));
    assert_eq!(receiver.seen().len(), 4);
    for (letter, record) in letters.iter().zip([&retried, &refused]) {
        let body: Value = serde_json::from_slice(&letter.body).unwrap();
        assert_eq!(body["data"], serde_json::to_value(record).unwrap());
    }
}

/// Keeps every record it is given.
#[derive(Default)]
struct Collect {
    records: Mutex<Vec<EventRecord>>,
}

#[async_trait]
impl EventSink for Collect {
    async fn emit(&self, _event: &KernelEvent) -> adk_kernel::Result<()> {
        unreachable!("the bus delivers records")
    }

    async fn emit_record(&self, record: &EventRecord) -> adk_kernel::Result<()> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner).push(record.clone());
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_receiver_never_blocks_the_bus_or_another_sink() {
    let receiver = Receiver::gated();
    let bus = EventBus::new(64);
    bus.add_sink(Arc::new(
        WebhookSink::new(&receiver.serve().await, secret())
            .unwrap()
            .with_filter(EventFilter::all()),
    ));
    let other = Arc::new(Collect::default());
    bus.add_sink(other.clone());
    let mut subscriber = bus.subscribe();

    // The receiver answers nothing yet: every publish still returns at once.
    let started = Instant::now();
    let published: Vec<EventRecord> =
        (0..40).map(|index| bus.publish(governance(index % 2 == 0))).collect();
    assert!(started.elapsed() < Duration::from_millis(250), "publishing waited on the receiver");
    for record in &published {
        let received = tokio::time::timeout(Duration::from_secs(1), subscriber.recv()).await;
        assert_eq!(received.unwrap().as_ref(), Some(record), "subscribers are not held up");
    }
    let count = || other.records.lock().unwrap_or_else(PoisonError::into_inner).len();
    eventually("the other sink", || count() == 40).await;
    assert!(receiver.seen().is_empty());

    // Once the receiver answers, every event arrives, in bus order.
    receiver.gate.as_ref().unwrap().add_permits(100);
    eventually("forty deliveries", || receiver.seen().len() == 40).await;
    let cursors: Vec<u64> = receiver
        .seen()
        .iter()
        .map(|delivery| delivery.json()["data"]["cursor"].as_u64().unwrap())
        .collect();
    let expected: Vec<u64> = published.iter().map(|record| record.cursor).collect();
    assert_eq!(cursors, expected);
}

/// A `MakeWriter` into a shared buffer.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn neither_the_secret_nor_the_urls_path_reaches_the_logs() {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::{Layer as _, filter::Targets};

    let logs = Captured::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .with_filter(
                Targets::new()
                    .with_target("adk_core", tracing::Level::TRACE)
                    .with_target("adk_kernel", tracing::Level::TRACE),
            ),
    );
    // The runtime runs every task on this thread, so the thread's subscriber sees them all.
    let _guard = tracing::subscriber::set_default(subscriber);

    // A closed port: every attempt fails to connect.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}/hooks/T0K3N-in-the-path?key=QUERY-S3CRET");
    let dead_letters = Arc::new(InMemoryDeadLetterSink::new());
    let sink = WebhookSink::new(&url, secret())
        .unwrap()
        .with_retry_policy(fast(2))
        .with_dead_letter_sink(dead_letters.clone());
    let bus = EventBus::new(16);
    bus.add_sink(Arc::new(sink));
    bus.publish(governance(true));
    eventually("the dead letter", || dead_letters.letters().len() == 1).await;
    eventually("the sink's warning", || logs.text().contains("kernel event not delivered")).await;

    let text = logs.text();
    assert!(text.contains(&format!("http://127.0.0.1:{port}")), "the origin is logged:\n{text}");
    assert!(text.contains("webhook dead-lettered"), "{text}");
    for hidden in ["T0K3N-in-the-path", "QUERY-S3CRET", SECRET, SECRET.trim_start_matches("whsec_")]
    {
        assert!(!text.contains(hidden), "{hidden} reached the logs:\n{text}");
    }
    // The dead letter keeps the full URL for a redrive; that is data, not a log line.
    assert_eq!(dead_letters.letters()[0].url, url);
}
