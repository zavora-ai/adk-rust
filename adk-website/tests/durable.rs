//! The product's durable state, payment records and spans (spec 3.5 §7.5, §7.9, §7.21,
//! §7.24, §9.3): the commerce and alarm stores pass their conformance checks in memory and in
//! SQLite and keep their records across a reopen; a restarted alarm router still repeats its
//! open alarm, sends its deferred one and reviews the waiting one; payments leave
//! `awp.payment.*` records; and signals and releases run in their `kernel.signal` and
//! `site.release` spans.

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use adk_awp::v2::capability::{CapabilityCall, CapabilityError, CapabilityHandler};
use adk_core::Clock as _;
use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_kernel::InMemoryAgendaStore;
use adk_kernel::charter::{InMemorySignalStore, SignalSink};
use adk_server::awp::SignalEmitter as _;
use adk_site::pipeline::{Change, EntryPoint, PipelineOutcome};
use adk_website::alarms::{Alarm, AlarmRouter, InMemoryAlarmStore, Routed, SqliteAlarmStore};
use adk_website::edge::adapters::KernelSignals;
use adk_website::edge::commerce::{
    CommerceStore, InMemoryCommerceStore, SqliteCommerceStore, StoredCalendar, conformance,
};
use adk_website::edge::handlers::{
    AWAITING_PAYMENT, Calendar, Order, OrderBook, PAID, PAYMENT_CREATED, PAYMENT_REJECTED,
    PAYMENT_SETTLED, PaymentDesk, PaymentHandler, Slot,
};
use adk_website::kinds::CharterInputs;
use adk_website::observability::spans;
use adk_website::onboarding::build::SitePlan;
use adk_website::team::AgentRole;
use async_trait::async_trait;
use awp_types::{
    Bounded, Cadence, Channel, ErrorCode, LeadRef, Reporting, Role, Signal, SignalKind, SiteKind,
    TrustTier, Urgency, UrgencyRoute,
};
use serde_json::{Value, json};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};
use uuid::Uuid;

/// A database file that a reopened store reads again.
struct File {
    _dir: tempfile::TempDir,
    url: String,
}

impl File {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.path().join("edge.db").display());
        Self { _dir: dir, url }
    }

    async fn pool(&self) -> sqlx::SqlitePool {
        sqlx::SqlitePool::connect(&self.url).await.unwrap()
    }

    async fn commerce(&self) -> Arc<dyn CommerceStore> {
        let store = SqliteCommerceStore::new(self.pool().await).with_site("bakery");
        store.migrate().await.unwrap();
        Arc::new(store)
    }

    async fn alarms(&self) -> SqliteAlarmStore {
        let store = SqliteAlarmStore::new(self.pool().await);
        store.migrate().await.unwrap();
        store
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commerce_stores_pass_the_conformance_check() {
    let memory: Arc<dyn CommerceStore> = Arc::new(InMemoryCommerceStore::default());
    conformance::commerce_store(|| {
        let memory = memory.clone();
        async move { memory }
    })
    .await;
    let file = File::new();
    conformance::commerce_store(|| file.commerce()).await;
}

#[tokio::test]
async fn a_booking_and_an_order_survive_a_reopen() {
    let file = File::new();
    let at = chrono::Utc::now() + chrono::Duration::days(1);
    let slots = || {
        [
            Slot { id: "mon-9".into(), starts_at: at, minutes: 30, price: Some(2_000) },
            Slot {
                id: "mon-10".into(),
                starts_at: at + chrono::Duration::hours(1),
                minutes: 30,
                price: None,
            },
        ]
    };
    let calendar = StoredCalendar::new(slots(), file.commerce().await);
    assert!(calendar.take("mon-9", "booking-1").await.unwrap());
    assert!(!calendar.take("tue-9", "booking-1").await.unwrap(), "no such slot");
    let orders = OrderBook::new(file.commerce().await);
    let quote: awp_types::QuoteDocument = serde_json::from_value(json!({
        "id": Uuid::new_v4(), "capability": "get_quote", "lines": [], "total": 900,
        "currency": "KES", "releaseSeq": 1, "issuedAt": at, "validUntil": at
    }))
    .unwrap();
    file.commerce()
        .await
        .place_order(&Order {
            id: "order-o-1".into(),
            quote,
            status: AWAITING_PAYMENT.into(),
            placed_at: at,
        })
        .await
        .unwrap();
    assert_eq!(orders.set_status("order-o-1", PAID).await.unwrap().status, PAID);

    // A restarted site reads both back.
    let calendar = StoredCalendar::new(slots(), file.commerce().await);
    let open = calendar.open(at - chrono::Duration::hours(1), at + chrono::Duration::days(1));
    let open: Vec<String> = open.await.unwrap().into_iter().map(|slot| slot.id).collect();
    assert_eq!(open, ["mon-10"], "the booked slot stays taken");
    assert!(calendar.take("mon-9", "booking-1").await.unwrap(), "the same booking again");
    assert!(!calendar.take("mon-9", "booking-2").await.unwrap());
    assert_eq!(calendar.price("mon-9"), Some(2_000));
    let order = OrderBook::new(file.commerce().await).get("order-o-1").await.unwrap();
    assert_eq!((order.status.as_str(), order.quote.total), (PAID, 900));
}

#[tokio::test]
async fn alarm_stores_pass_the_conformance_check() {
    adk_website::alarms::conformance::alarm_store(&InMemoryAlarmStore::default()).await;
    adk_website::alarms::conformance::alarm_store(&File::new().alarms().await).await;
}

#[tokio::test]
async fn a_restarted_router_keeps_its_open_deferred_and_waiting_alarms() {
    let file = File::new();
    let site = Uuid::new_v4();
    let start = chrono::DateTime::parse_from_rfc3339("2026-10-26T23:00:00Z").unwrap().to_utc();
    let clock = Arc::new(adk_core::ManualClock::new(start));
    let reporting = Reporting {
        review: Cadence::Weekly,
        routes: vec![
            UrgencyRoute { urgency: Urgency::P1, channels: vec![Channel::Sms] },
            UrgencyRoute { urgency: Urgency::P2, channels: vec![Channel::Sms] },
        ],
        quiet_hours: Some(awp_types::QuietHours {
            start: chrono::NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
            end: chrono::NaiveTime::from_hms_opt(7, 0, 0).unwrap(),
            timezone: "UTC".into(),
        }),
    };
    let phone = common::Phone::new("sms");
    let router = |store: SqliteAlarmStore| {
        AlarmRouter::new(site, &reporting)
            .with_sink(phone.clone(), "owner")
            .with_clock(clock.clone())
            .with_store(Arc::new(store))
    };
    let first = router(file.alarms().await);
    let now = clock.now();
    let down = first.raise(Alarm::new(site, Urgency::P1, "incident", "The site is down", now));
    assert!(matches!(down.await, Routed::Sent { .. }));
    let held = first.raise(Alarm::new(site, Urgency::P2, "held", "A buyer waits", now)).await;
    assert_eq!(held, Routed::Deferred, "inside quiet hours");
    let stale = first.raise(Alarm::new(site, Urgency::P3, "freshness", "Two stale pages", now));
    assert_eq!(stale.await, Routed::Review);
    drop(first);

    // The restarted router knows all three, and numbers the next one after them.
    let second = router(file.alarms().await);
    assert_eq!(second.open().await.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["alarm-1"]);
    assert_eq!(second.deferred().await[0].id, "alarm-2");
    assert_eq!(second.review_items().await[0].id, "alarm-3");
    second.raise(Alarm::new(site, Urgency::P3, "seo", "A title is long", now)).await;
    assert_eq!(second.review_items().await[1].id, "alarm-4");
    clock.advance(std::time::Duration::from_secs(9 * 3600));
    let sent: Vec<String> = second.tick().await.into_iter().map(|d| d.alarm).collect();
    assert_eq!(sent, ["alarm-1", "alarm-2"], "the open alarm repeats, the deferred one goes");
    second.acknowledge("alarm-1", &adk_core::principal::SubjectRef::human("amina")).await.unwrap();
    assert_eq!(second.take_review_items().await.len(), 2);
    drop(second);

    // Nothing is left to send or review after a second restart.
    let third = router(file.alarms().await);
    assert!(third.open().await.is_empty() && third.deferred().await.is_empty());
    assert!(third.review_items().await.is_empty());
    assert_eq!(third.raised_since(start).await.len(), 4);
    assert_eq!(phone.messages().len(), 3, "the first send, the repeat, the deferred, nothing more");
}

/// A desk that answers each payment as told.
struct Desk(Result<&'static str, &'static str>);

#[async_trait]
impl PaymentDesk for Desk {
    async fn take(&self, _call: &CapabilityCall) -> Result<Value, CapabilityError> {
        match self.0 {
            Ok(status) => Ok(json!({ "status": status, "receipt": "r-1" })),
            Err(message) => Err(CapabilityError::Refused {
                code: ErrorCode::PaymentPolicyRejected,
                message: message.into(),
            }),
        }
    }
}

fn pay(order: &str, key: &str) -> CapabilityCall {
    CapabilityCall {
        capability: "pay_order".into(),
        input: json!({ "orderId": order, "rail": "card" }),
        tier: TrustTier::Known,
        requester: Some(awp_types::RequesterId::new("key:buyer")),
        idempotency_key: key.into(),
        consent: Vec::new(),
    }
}

#[tokio::test]
async fn payments_leave_created_settled_and_rejected_records() {
    let recorder = InMemoryAuditRecorder::new();
    let trail = AuditTrail::new(Arc::new(recorder.clone()));
    let store: Arc<dyn CommerceStore> = Arc::new(InMemoryCommerceStore::default());
    let at = chrono::Utc::now();
    for id in ["order-a", "order-b"] {
        let quote: awp_types::QuoteDocument = serde_json::from_value(json!({
            "id": Uuid::new_v4(), "capability": "get_quote", "lines": [], "total": 900,
            "currency": "KES", "releaseSeq": 1, "issuedAt": at, "validUntil": at
        }))
        .unwrap();
        let order = Order { id: id.into(), quote, status: AWAITING_PAYMENT.into(), placed_at: at };
        store.place_order(&order).await.unwrap();
    }
    let orders = Arc::new(OrderBook::new(store));
    let handler = |desk: Desk| {
        PaymentHandler::new(Some(Arc::new(desk) as Arc<dyn PaymentDesk>))
            .with_orders(orders.clone())
            .with_audit(trail.clone())
    };
    handler(Desk(Ok(PAID))).handle(pay("order-a", "p-1")).await.unwrap();
    assert!(handler(Desk(Err("card declined"))).handle(pay("order-b", "p-2")).await.is_err());
    assert_eq!(orders.get("order-a").await.unwrap().status, PAID);
    assert_eq!(orders.get("order-b").await.unwrap().status, AWAITING_PAYMENT, "free to retry");

    let records = recorder.records();
    let actions: Vec<&str> = records.iter().map(|r| r.action.as_str()).collect();
    assert_eq!(actions, [PAYMENT_CREATED, PAYMENT_SETTLED, PAYMENT_CREATED, PAYMENT_REJECTED]);
    assert_eq!(records[1].ext["order"], "order-a");
    assert_eq!(
        (records[1].ext["amount"].clone(), records[1].ext["currency"].clone()),
        (json!(900), json!("KES"))
    );
    assert_eq!(records[1].ext["requester"], "key:buyer");
    assert!(records[3].ext["error"].as_str().unwrap().contains("card declined"));
}

/// A span's id, name and fields as recorded.
type Opened = (u64, String, BTreeMap<String, String>);

/// Every span opened.
#[derive(Clone, Default)]
struct Spans(Arc<Mutex<Vec<Opened>>>);

struct Fields<'a>(&'a mut BTreeMap<String, String>);

impl Visit for Fields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.insert(field.name().to_string(), format!("{value:?}"));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
}

impl<S: tracing::Subscriber> Layer<S> for Spans {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, _: Context<'_, S>) {
        let mut fields = BTreeMap::new();
        attrs.record(&mut Fields(&mut fields));
        self.0.lock().unwrap().push((id.into_u64(), attrs.metadata().name().into(), fields));
    }
    fn on_record(&self, id: &Id, values: &Record<'_>, _: Context<'_, S>) {
        let mut spans = self.0.lock().unwrap();
        if let Some((_, _, fields)) = spans.iter_mut().find(|(span, _, _)| *span == id.into_u64()) {
            values.record(&mut Fields(fields));
        }
    }
}

impl Spans {
    fn named(&self, name: &str) -> Vec<BTreeMap<String, String>> {
        let spans = self.0.lock().unwrap();
        spans.iter().filter(|(_, span, _)| span == name).map(|(_, _, f)| f.clone()).collect()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn signals_and_releases_run_in_their_spans() {
    let spans = Spans::default();
    let _guard =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(spans.clone()));
    assert_eq!(spans::SITE_RELEASE, adk_site::pipeline::RELEASE_SPAN);

    let sink = SignalSink::new(
        Arc::new(InMemorySignalStore::new()),
        Arc::new(InMemoryAgendaStore::new()),
        "back-office",
    );
    let signal = Signal {
        id: Uuid::new_v4(),
        kind: SignalKind::LeadCaptured {
            lead_ref: LeadRef("lead-1".into()),
            source: Channel::Http,
            consent: Vec::new(),
            answers: [("message".to_string(), Bounded::truncated("Wedding cakes?", 280))]
                .into_iter()
                .collect(),
        },
        emitted_by: "urn:adk:agent:juniper/agent-desk".into(),
        emitted_at: chrono::Utc::now(),
    };
    KernelSignals::new(sink).emit(signal.clone()).await.unwrap();
    let filed = spans.named(spans::KERNEL_SIGNAL);
    assert_eq!(filed.len(), 1, "{filed:?}");
    assert_eq!(filed[0]["signal.kind"], "lead_captured");
    assert_eq!(filed[0]["signal.id"], signal.id.to_string());

    let site =
        common::Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let inputs = CharterInputs::new(
        site.site.id,
        "Juniper Bakery",
        common::OWNER,
        "Fresh bread every morning.",
        vec!["en".parse().unwrap()],
        "KES",
    );
    let charter = site.template.draft_charter(&site.team, &inputs);
    site.commit(&charter, &[(Role::Owner, common::OWNER)]).await;
    let items = SitePlan::new(&charter, "Juniper Bakery").items(&charter, &[], &["KE".into()]);
    let change = Change {
        items,
        media: Vec::new(),
        entry: EntryPoint::SiteAgent { agent: AgentRole::Steward.principal(&site.site) },
        proposer: AgentRole::Steward.principal(&site.site),
        job: "onboarding".into(),
        action_class: "publish".into(),
    };
    let outcome = site.runtime.pipeline().run(change).await.unwrap();
    assert!(matches!(outcome, PipelineOutcome::Published(_)), "{outcome:?}");
    let releases = spans.named(spans::SITE_RELEASE);
    assert_eq!(releases.len(), 1, "{releases:?}");
    assert_eq!(
        (releases[0]["site.release.seq"].as_str(), releases[0]["site.release.source"].as_str()),
        ("1", "onboarding")
    );
    // The runtime's audit trail holds the pipeline's records.
    let actions: Vec<String> = site.audit.records().into_iter().map(|r| r.action).collect();
    for action in ["site.release.built", "site.release.signed", "site.content.published"] {
        assert!(actions.iter().any(|a| a == action), "{action} in {actions:?}");
    }
}
