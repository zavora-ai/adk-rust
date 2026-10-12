//! Queue wake-ups and signed outbound delivery.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::conformance::QueueSourceHarness;
use adk_core::queue_source::{InMemoryQueueSource, QueueIngress, QueueSource};
use adk_core::run_store::{
    ClaimedRun, DurableRunStore, Enqueued, InMemoryRunStore, NewRun, OnConflict, RunCheckpointRef,
    RunFilter, RunOutcome, RunRecord,
};
use adk_core::webhook::{WebhookHeaders, WebhookSecret, WebhookVerifier};
use adk_core::webhook_delivery::{
    DeliveryReport, EgressPolicy, InMemoryDeadLetterSink, RetryPolicy, WebhookDelivery,
    WebhookEndpoint, WebhookMessage,
};
use adk_core::{AdkError, GovernanceControl, ManualClock, Result, async_trait};
use chrono::{DateTime, Utc};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ─── QueueSource conformance ────────────────────────────────────────────────

struct InMemoryQueue {
    clock: ManualClock,
    source: Arc<InMemoryQueueSource>,
}

impl InMemoryQueue {
    fn new() -> Self {
        let clock = ManualClock::new(Utc::now());
        let source =
            Arc::new(InMemoryQueueSource::new("conformance").with_clock(Arc::new(clock.clone())));
        Self { clock, source }
    }
}

#[async_trait]
impl QueueSourceHarness for InMemoryQueue {
    async fn open(&self) -> Arc<dyn QueueSource> {
        self.source.clone()
    }

    async fn publish(&self, id: &str, body: &[u8]) {
        self.source.publish(id, body.to_vec());
    }

    async fn lapse_leases(&self) {
        self.clock.advance(Duration::from_secs(31));
    }

    fn pull_wait(&self) -> Duration {
        Duration::ZERO
    }
}

mod queue_conformance {
    adk_core::queue_source_conformance_tests!(harness = super::InMemoryQueue::new());
}

// ─── Queue ingress ──────────────────────────────────────────────────────────

/// A run store whose next `fail_next` enqueues fail, as an unreachable database would.
struct FlakyStore {
    inner: InMemoryRunStore,
    fail_next: AtomicUsize,
}

#[async_trait]
impl DurableRunStore for FlakyStore {
    async fn enqueue(&self, run: NewRun) -> Result<Enqueued> {
        if self
            .fail_next
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(AdkError::session("run store unreachable"));
        }
        self.inner.enqueue(run).await
    }
    async fn claim(&self, queue: &str, worker: &str, ttl: Duration) -> Result<Option<ClaimedRun>> {
        self.inner.claim(queue, worker, ttl).await
    }
    async fn heartbeat(&self, run_id: &str, epoch: u64, ttl: Duration) -> Result<bool> {
        self.inner.heartbeat(run_id, epoch, ttl).await
    }
    async fn set_checkpoint(
        &self,
        run_id: &str,
        epoch: u64,
        checkpoint: RunCheckpointRef,
    ) -> Result<()> {
        self.inner.set_checkpoint(run_id, epoch, checkpoint).await
    }
    async fn wait(&self, run_id: &str, epoch: u64, until: Option<DateTime<Utc>>) -> Result<()> {
        self.inner.wait(run_id, epoch, until).await
    }
    async fn wake(&self, run_id: &str) -> Result<()> {
        self.inner.wake(run_id).await
    }
    async fn finish(&self, run_id: &str, epoch: u64, outcome: RunOutcome) -> Result<RunRecord> {
        self.inner.finish(run_id, epoch, outcome).await
    }
    async fn request_cancel(&self, run_id: &str) -> Result<()> {
        self.inner.request_cancel(run_id).await
    }
    async fn get(&self, run_id: &str) -> Result<Option<RunRecord>> {
        self.inner.get(run_id).await
    }
    async fn list(&self, filter: RunFilter) -> Result<Vec<RunRecord>> {
        self.inner.list(filter).await
    }
    async fn prune(&self, finished_before: DateTime<Utc>) -> Result<u64> {
        self.inner.prune(finished_before).await
    }
}

fn order_run(message: &adk_core::QueueMessage) -> Result<NewRun> {
    Ok(NewRun::new("default", "shop", "system", "order_agent", message.body_json()?))
}

struct Fixture {
    clock: ManualClock,
    source: Arc<InMemoryQueueSource>,
    store: Arc<FlakyStore>,
}

impl Fixture {
    fn new() -> Self {
        let clock = ManualClock::new(Utc::now());
        let source =
            Arc::new(InMemoryQueueSource::new("orders").with_clock(Arc::new(clock.clone())));
        let store = Arc::new(FlakyStore {
            inner: InMemoryRunStore::new().with_clock(Arc::new(clock.clone())),
            fail_next: AtomicUsize::new(0),
        });
        Self { clock, source, store }
    }

    fn ingress(&self) -> QueueIngress {
        QueueIngress::new(self.source.clone(), self.store.clone(), order_run)
            .with_pull_wait(Duration::ZERO)
            .with_retry_after(Duration::from_secs(5))
    }

    async fn runs(&self) -> Vec<RunRecord> {
        self.store.list(RunFilter::default()).await.unwrap()
    }
}

#[tokio::test]
async fn a_queued_message_is_enqueued_then_acked() {
    let fixture = Fixture::new();
    fixture.source.publish("m-1", br#"{"orderId": 7}"#.to_vec());
    let report = fixture.ingress().poll_once().await.unwrap();
    assert_eq!((report.pulled, report.created.len(), report.nacked), (1, 1, 0));
    let runs = fixture.runs().await;
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].input, json!({"orderId": 7}));
    assert_eq!(runs[0].idempotency_key.as_deref(), Some("queue:orders:m-1"));
    assert_eq!(fixture.source.acked(), vec!["m-1".to_string()]);
    assert_eq!(fixture.source.unacked(), 0);
}

#[tokio::test]
async fn a_failed_enqueue_nacks_and_the_redelivery_creates_one_run() {
    let fixture = Fixture::new();
    fixture.store.fail_next.store(1, Ordering::SeqCst);
    fixture.source.publish("m-1", br#"{"orderId": 7}"#.to_vec());
    let ingress = fixture.ingress();

    let failed = ingress.poll_once().await.unwrap();
    assert_eq!((failed.pulled, failed.nacked), (1, 1));
    assert!(fixture.runs().await.is_empty());
    assert_eq!(fixture.source.unacked(), 1, "nacked, not acked");

    // Not redelivered before the retry delay.
    assert_eq!(ingress.poll_once().await.unwrap().pulled, 0);
    fixture.clock.advance(Duration::from_secs(5));
    let redelivered = ingress.poll_once().await.unwrap();
    assert_eq!((redelivered.pulled, redelivered.created.len()), (1, 1));
    assert_eq!(fixture.runs().await.len(), 1);
    assert_eq!(fixture.source.unacked(), 0);
}

#[tokio::test]
async fn an_ack_lost_after_enqueue_redelivers_into_the_same_run() {
    let fixture = Fixture::new();
    fixture.source.publish("m-1", br#"{"orderId": 7}"#.to_vec());
    // The worker crashes after enqueue, before ack: simulate by enqueueing through a
    // pulled-but-never-acked message, then letting the lease lapse.
    let pulled = fixture.source.pull(1, Duration::ZERO).await.unwrap().remove(0);
    let mut run = order_run(&pulled).unwrap();
    run.idempotency_key = Some(QueueIngress::idempotency_key("orders", &pulled.id));
    let first = fixture.store.enqueue(run).await.unwrap();
    fixture.clock.advance(Duration::from_secs(31));

    let report = fixture.ingress().poll_once().await.unwrap();
    assert_eq!((report.pulled, report.created.len(), report.existing), (1, 0, 1));
    let runs = fixture.runs().await;
    assert_eq!(runs.len(), 1);
    assert_eq!(Some(&runs[0]), first.record());
    assert_eq!(fixture.source.unacked(), 0);
}

#[tokio::test]
async fn duplicate_deliveries_create_one_run() {
    let fixture = Fixture::new();
    fixture.source.publish("m-1", br#"{"orderId": 7}"#.to_vec());
    fixture.source.publish("m-1", br#"{"orderId": 7}"#.to_vec());
    let report = fixture.ingress().poll_once().await.unwrap();
    assert_eq!((report.created.len(), report.existing), (1, 1));
    assert_eq!(fixture.runs().await.len(), 1);
    assert_eq!(fixture.source.unacked(), 0);
}

#[tokio::test]
async fn a_frozen_kill_switch_holds_the_queue_until_unfrozen() {
    let fixture = Fixture::new();
    let governance = GovernanceControl::new();
    governance.freeze("incident 7");
    fixture.source.publish("m-1", br#"{"orderId": 7}"#.to_vec());
    let ingress = fixture.ingress().with_governance(governance.clone());

    let held = ingress.poll_once().await.unwrap();
    assert_eq!(held.held.as_deref(), Some("governance.frozen"));
    assert_eq!(held.pulled, 0);
    assert!(fixture.runs().await.is_empty());

    governance.unfreeze();
    let report = ingress.poll_once().await.unwrap();
    assert_eq!(report.created.len(), 1);
}

#[tokio::test]
async fn a_freeze_that_lands_mid_batch_nacks_the_rest() {
    let fixture = Fixture::new();
    let governance = GovernanceControl::new();
    let freeze_after_first = governance.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let ingress =
        QueueIngress::new(fixture.source.clone(), fixture.store.clone(), move |message| {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                freeze_after_first.freeze("operator froze mid-batch");
            }
            order_run(message)
        })
        .with_governance(governance)
        .with_pull_wait(Duration::ZERO);
    fixture.source.publish("m-1", br#"{}"#.to_vec());
    fixture.source.publish("m-2", br#"{}"#.to_vec());
    let report = ingress.poll_once().await.unwrap();
    // The check runs after mapping, immediately before the enqueue.
    assert_eq!(report.pulled, 2);
    assert_eq!(report.nacked, 2, "every message after the freeze is nacked");
    assert!(fixture.runs().await.is_empty());
    assert_eq!(fixture.source.unacked(), 2);
}

#[tokio::test]
async fn a_rejected_concurrency_key_nacks_for_later() {
    let fixture = Fixture::new();
    let ingress = QueueIngress::new(fixture.source.clone(), fixture.store.clone(), |message| {
        Ok(order_run(message)?.with_concurrency_key("customer:1", OnConflict::Reject))
    })
    .with_pull_wait(Duration::ZERO);
    fixture.source.publish("m-1", br#"{}"#.to_vec());
    fixture.source.publish("m-2", br#"{}"#.to_vec());
    let report = ingress.poll_once().await.unwrap();
    assert_eq!((report.created.len(), report.nacked), (1, 1));
    assert_eq!(fixture.source.unacked(), 1);
}

#[tokio::test]
async fn max_pending_holds_pulls_until_runs_finish() {
    let fixture = Fixture::new();
    let ingress = fixture.ingress().with_max_pending(1);
    fixture.source.publish("m-1", br#"{}"#.to_vec());
    fixture.source.publish("m-2", br#"{}"#.to_vec());
    let first = ingress.poll_once().await.unwrap();
    assert_eq!((first.pulled, first.created.len()), (1, 1), "the batch shrinks to the room left");
    let held = ingress.poll_once().await.unwrap();
    assert_eq!(held.held.as_deref(), Some("queue.max_pending"));
    assert_eq!(fixture.source.unacked(), 1);

    let claimed =
        fixture.store.claim("default", "w", Duration::from_secs(30)).await.unwrap().unwrap();
    fixture
        .store
        .finish(&claimed.record.run_id, claimed.epoch, RunOutcome::Completed(json!(null)))
        .await
        .unwrap();
    assert_eq!(ingress.poll_once().await.unwrap().created.len(), 1);
    assert_eq!(fixture.source.unacked(), 0);
}

#[tokio::test]
async fn an_unparseable_message_is_nacked_not_dropped() {
    let fixture = Fixture::new();
    fixture.source.publish("m-1", b"not json".to_vec());
    let report = fixture.ingress().poll_once().await.unwrap();
    assert_eq!(report.nacked, 1);
    assert_eq!(fixture.source.unacked(), 1);
}

// ─── Outbound delivery ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Received {
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// A local HTTP endpoint that answers each request with the next scripted status.
async fn scripted_endpoint(
    statuses: Vec<(u16, Option<&'static str>)>,
) -> (String, Arc<std::sync::Mutex<Vec<Received>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = received.clone();
    tokio::spawn(async move {
        let mut statuses = statuses.into_iter();
        let mut last = (200, None);
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let (status, retry_after) = statuses.next().unwrap_or(last);
            last = (status, retry_after);
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 4096];
            let (head_end, length) = loop {
                let n = socket.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break (None, 0);
                }
                buffer.extend_from_slice(&chunk[..n]);
                if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buffer[..end]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    break (Some(end + 4), length);
                }
            };
            let Some(head_end) = head_end else { continue };
            while buffer.len() < head_end + length {
                let n = socket.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buffer.extend_from_slice(&chunk[..n]);
            }
            let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
            let headers = head
                .lines()
                .skip(1)
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
                .collect();
            log.lock().unwrap().push(Received { headers, body: buffer[head_end..].to_vec() });
            let extra =
                retry_after.map(|value| format!("retry-after: {value}\r\n")).unwrap_or_default();
            let response = format!(
                "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n{extra}\r\n"
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (url, received)
}

fn secret() -> WebhookSecret {
    WebhookSecret::from_bytes(b"outbound-secret-outbound-secret!".to_vec()).unwrap()
}

fn fast_retries(max_attempts: u32) -> RetryPolicy {
    RetryPolicy::default()
        .with_max_attempts(max_attempts)
        .with_initial_backoff(Duration::from_millis(10))
        .with_max_backoff(Duration::from_millis(40))
}

fn local_delivery(max_attempts: u32, sink: Arc<InMemoryDeadLetterSink>) -> WebhookDelivery {
    WebhookDelivery::new()
        .with_retry_policy(fast_retries(max_attempts))
        .with_egress_policy(EgressPolicy::allow_private_networks())
        .with_dead_letter_sink(sink)
}

#[tokio::test]
async fn outbound_retries_until_delivered_with_one_id_and_valid_signatures() {
    let (url, received) = scripted_endpoint(vec![(503, None), (500, None), (200, None)]).await;
    let sink = Arc::new(InMemoryDeadLetterSink::new());
    let message =
        WebhookMessage::json("evt_1", &json!({"runId": "r1", "state": "completed"})).unwrap();
    let report = local_delivery(5, sink.clone())
        .deliver(&WebhookEndpoint::new(&url, secret()), &message)
        .await;
    assert_eq!(report, DeliveryReport::Delivered { attempts: 3, status: 200 });
    assert!(sink.letters().is_empty());

    let received = received.lock().unwrap().clone();
    assert_eq!(received.len(), 3);
    let verifier = WebhookVerifier::new(secret());
    for attempt in &received {
        assert_eq!(attempt.headers["webhook-id"], "evt_1", "the id is stable across retries");
        assert_eq!(attempt.body, message.body);
        let headers =
            WebhookHeaders::from_lookup(|name| attempt.headers.get(name).map(String::as_str))
                .unwrap();
        verifier.verify(&headers, &attempt.body).expect("every attempt is signed");
    }
}

#[tokio::test]
async fn outbound_dead_letters_after_the_last_attempt() {
    let (url, received) = scripted_endpoint(vec![(500, None)]).await;
    let sink = Arc::new(InMemoryDeadLetterSink::new());
    let message = WebhookMessage::json("evt_2", &json!({"n": 2})).unwrap();
    let report = local_delivery(3, sink.clone())
        .deliver(&WebhookEndpoint::new(&url, secret()), &message)
        .await;
    let DeliveryReport::DeadLettered(letter) = report else { panic!("expected a dead letter") };
    assert_eq!((letter.attempts, letter.last_status), (3, Some(500)));
    assert_eq!(received.lock().unwrap().len(), 3);
    assert_eq!(sink.letters(), vec![letter]);
}

#[tokio::test]
async fn outbound_permanent_failure_dead_letters_without_retrying() {
    let (url, received) = scripted_endpoint(vec![(410, None)]).await;
    let sink = Arc::new(InMemoryDeadLetterSink::new());
    let message = WebhookMessage::json("evt_3", &json!({})).unwrap();
    let report = local_delivery(5, sink.clone())
        .deliver(&WebhookEndpoint::new(&url, secret()), &message)
        .await;
    assert!(
        matches!(report, DeliveryReport::DeadLettered(ref letter) if letter.attempts == 1 && letter.last_status == Some(410))
    );
    assert_eq!(received.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn outbound_honours_retry_after_up_to_the_cap() {
    let (url, received) = scripted_endpoint(vec![(429, Some("1")), (200, None)]).await;
    let sink = Arc::new(InMemoryDeadLetterSink::new());
    let delivery = WebhookDelivery::new()
        .with_retry_policy(fast_retries(3).with_max_backoff(Duration::from_millis(300)))
        .with_egress_policy(EgressPolicy::allow_private_networks())
        .with_dead_letter_sink(sink);
    let started = std::time::Instant::now();
    let message = WebhookMessage::json("evt_4", &json!({})).unwrap();
    let report = delivery.deliver(&WebhookEndpoint::new(&url, secret()), &message).await;
    assert_eq!(report, DeliveryReport::Delivered { attempts: 2, status: 200 });
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(300),
        "Retry-After raised the wait to the cap: {waited:?}"
    );
    assert!(waited < Duration::from_secs(1), "Retry-After is capped at max_backoff: {waited:?}");
    assert_eq!(received.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn outbound_ssrf_targets_are_refused_before_any_request() {
    let (url, received) = scripted_endpoint(vec![(200, None)]).await;
    let sink = Arc::new(InMemoryDeadLetterSink::new());
    // The default egress policy: HTTPS to public addresses only.
    let delivery = WebhookDelivery::new()
        .with_retry_policy(fast_retries(3))
        .with_dead_letter_sink(sink.clone());
    let message = WebhookMessage::json("evt_5", &json!({})).unwrap();
    let https_local = url.replace("http://", "https://");
    for target in [
        url.as_str(),
        https_local.as_str(),
        "https://169.254.169.254/latest/meta-data",
        "https://localhost/hook",
        "https://[::ffff:10.0.0.1]/hook",
    ] {
        let report = delivery.deliver(&WebhookEndpoint::new(target, secret()), &message).await;
        assert!(
            matches!(report, DeliveryReport::DeadLettered(ref letter) if letter.attempts == 0),
            "{target}: {report:?}"
        );
    }
    assert!(received.lock().unwrap().is_empty(), "no request reached the internal endpoint");
    assert_eq!(sink.letters().len(), 5);
}

#[tokio::test]
async fn outbound_signatures_carry_every_rotation_key() {
    let (url, received) = scripted_endpoint(vec![(200, None)]).await;
    let next = WebhookSecret::from_bytes(b"next-secret-next-secret-next-sec".to_vec()).unwrap();
    let signer = adk_core::webhook::WebhookSigner::new(secret()).with_secret(next.clone());
    let delivery = local_delivery(1, Arc::new(InMemoryDeadLetterSink::new()));
    let message = WebhookMessage::json("evt_6", &json!({})).unwrap();
    let report = delivery.deliver(&WebhookEndpoint::with_signer(&url, signer), &message).await;
    assert!(report.is_delivered());
    let attempt = received.lock().unwrap()[0].clone();
    let headers =
        WebhookHeaders::from_lookup(|name| attempt.headers.get(name).map(String::as_str)).unwrap();
    // A receiver that already moved to the next key accepts the delivery.
    WebhookVerifier::new(next).verify(&headers, &attempt.body).unwrap();
}
