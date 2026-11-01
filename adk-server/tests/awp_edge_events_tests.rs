//! The edge's events, consent, status and beacon (spec 3.5 §7.5, §7.12, A-I8): Standard
//! Webhooks subscriptions with retries, suspension and replay, coalesced catalogue changes,
//! signed consent receipts whose revocation stops pending calls, signed status transitions, and
//! the cookieless beacon.

#![cfg(feature = "awp")]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_auth::web_bot_auth::{
    Ed25519SigningKey, HttpSignatureSigner, HttpSignatureVerifier, InMemoryKeyDirectorySource,
    KeyDirectory,
};
use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner, verify};
use adk_awp::v2::capability::{
    CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry,
};
use adk_awp::v2::store::{Lead, LeadStore, MemoryEdgeStores};
use adk_core::webhook::{WebhookHeaders, WebhookSecret, WebhookVerifier};
use adk_core::webhook_delivery::EgressPolicy;
use adk_core::{GovernanceControl, Url};
use adk_server::awp::{
    AwpEdge, ConsentConfig, ConsentPolicy, EdgeKeys, EdgeMode, EdgeStores, EdgeTiers, EventsConfig,
    WebBotAuthTrustAssigner,
};
use async_trait::async_trait;
use awp_types::{
    ApprovalHint, CapabilityEffect, CapabilityV2, Channel, ConsentReceipt, ConsentReceiptRef,
    ConsentRequirement, ConsentScope, ContactRef, ContentId, Digest, EventType, LeadRef,
    OrderUpdated, RequesterId, TrustTier,
};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, Request, StatusCode, header};
use chrono::Utc;
use serde_json::{Value, json};
use tower::ServiceExt;

const EDGE: &str = "https://agent.example.com";
const BUYER: &str = "https://buyer.example";
const OTHER: &str = "https://other-buyer.example";

struct Callback;

#[async_trait]
impl CapabilityHandler for Callback {
    async fn handle(&self, _call: CapabilityCall) -> Result<Value, CapabilityError> {
        Ok(json!({ "scheduled": true }))
    }
}

/// Stops a spawned server on every exit path.
struct Serving(tokio::task::JoinHandle<()>);

impl Drop for Serving {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A webhook receiver on loopback that records every delivery and answers `status`.
struct Receiver {
    url: String,
    hits: Arc<Mutex<Vec<(HeaderMap, Bytes)>>>,
    status: Arc<AtomicU16>,
    _serving: Serving,
}

impl Receiver {
    async fn start() -> Self {
        let hits: Arc<Mutex<Vec<(HeaderMap, Bytes)>>> = Arc::default();
        let status = Arc::new(AtomicU16::new(200));
        let (record, answer) = (Arc::clone(&hits), Arc::clone(&status));
        let app = Router::new().route(
            "/hook",
            axum::routing::post(move |headers: HeaderMap, body: Bytes| {
                let (record, answer) = (Arc::clone(&record), Arc::clone(&answer));
                async move {
                    record.lock().unwrap().push((headers, body));
                    StatusCode::from_u16(answer.load(Ordering::SeqCst)).unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let serving =
            Serving(tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }));
        Self { url, hits, status, _serving: serving }
    }

    /// Waits up to five seconds for at least `count` deliveries.
    async fn wait_for(&self, count: usize) -> Vec<(HeaderMap, Value)> {
        for _ in 0..500 {
            if self.hits.lock().unwrap().len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.hits
            .lock()
            .unwrap()
            .iter()
            .map(|(headers, body)| (headers.clone(), serde_json::from_slice(body).unwrap()))
            .collect()
    }
}

struct Site {
    edge: AwpEdge,
    router: Router,
    buyer: HttpSignatureSigner,
    other: HttpSignatureSigner,
    buyer_id: RequesterId,
    receipt_key: Arc<Ed25519DocumentSigner>,
}

async fn site(events: EventsConfig) -> Site {
    site_over(events, &MemoryEdgeStores::default()).await
}

/// The site over `stores`, shared with the caller.
async fn site_over(events: EventsConfig, stores: &MemoryEdgeStores) -> Site {
    let mut registry = CapabilityRegistry::new("example.com");
    let mut callback =
        CapabilityV2::new("request_callback", "Ask for a call back", CapabilityEffect::Idempotent)
            .with_min_trust(TrustTier::Known)
            .with_approval(ApprovalHint::Always);
    callback.consent = Some(ConsentRequirement {
        scope: ConsentScope::ProactiveOutreach,
        policy_version: "2026-10".into(),
    });
    registry.declare(callback, Arc::new(Callback)).unwrap();

    let directories = Arc::new(InMemoryKeyDirectorySource::new());
    let mut signers = Vec::new();
    for origin in [BUYER, OTHER] {
        let key = Arc::new(Ed25519SigningKey::generate().unwrap());
        directories.insert(origin, KeyDirectory::from_keys([key.as_ref()]), None).await;
        signers
            .push((HttpSignatureSigner::builder(Arc::clone(&key), origin).build().unwrap(), key));
    }
    let buyer_id = RequesterId::web_bot_auth(BUYER, signers[0].1.thumbprint());
    let receipt_key = Arc::new(Ed25519DocumentSigner::from_seed("edge-receipt", [8; 32]).unwrap());
    let keys = EdgeKeys {
        card: Arc::new(Ed25519DocumentSigner::from_seed("edge-card", [7; 32]).unwrap()),
        receipt: Arc::clone(&receipt_key) as Arc<dyn DocumentSigner>,
    };
    let consent = ConsentConfig::new(
        vec![
            ConsentScope::ProactiveOutreach,
            ConsentScope::PersistentProfile,
            ConsentScope::Newsletter,
        ],
        BTreeMap::from([(
            "2026-10".to_owned(),
            ConsentPolicy { digest: Digest::from_sha256_bytes([1; 32]), ttl: None },
        )]),
        vec![9_u8; 32],
    );
    let edge = AwpEdge::builder(registry, keys, Url::parse(EDGE).unwrap())
        .tiers(
            EdgeTiers::builder()
                .verifier(WebBotAuthTrustAssigner::new(
                    HttpSignatureVerifier::builder(directories).build(),
                ))
                .build(),
        )
        .events(events)
        .consent(consent)
        .stores(EdgeStores::in_memory(stores))
        .build()
        .unwrap();
    let mut signers = signers.into_iter().map(|(signer, _)| signer);
    Site {
        router: edge.router(),
        edge,
        buyer: signers.next().unwrap(),
        other: signers.next().unwrap(),
        buyer_id,
        receipt_key,
    }
}

fn local_events() -> EventsConfig {
    EventsConfig::default()
        .egress(EgressPolicy::allow_private_networks())
        .retry_schedule(vec![Duration::from_millis(5); 6])
}

fn request(method: &str, path: &str, body: Option<Value>) -> Request<Body> {
    let builder = Request::builder().method(method).uri(format!("{EDGE}{path}"));
    match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

impl Site {
    async fn send(&self, request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 22).await.unwrap();
        (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn as_buyer(&self, mut request: Request<Body>) -> (StatusCode, HeaderMap, Value) {
        self.buyer.sign_request(&mut request).unwrap();
        self.send(request).await
    }

    async fn subscribe(&self, url: &str, types: &[&str]) -> Value {
        let (status, headers, body) = self
            .as_buyer(request(
                "POST",
                "/awp/v2/events",
                Some(json!({ "types": types, "url": url })),
            ))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        assert_eq!(
            headers[header::LOCATION],
            format!("/awp/v2/events/{}", body["id"].as_str().unwrap()).as_str()
        );
        body
    }
}

fn verifier(subscription: &Value) -> WebhookVerifier {
    WebhookVerifier::new(
        WebhookSecret::from_base64(subscription["secret"].as_str().unwrap()).unwrap(),
    )
}

/// The delivery's raw body verifies with the subscription's secret and names its event.
fn verify_delivery(verifier: &WebhookVerifier, headers: &HeaderMap, raw: &[u8], body: &Value) {
    let lookup = |name: &'static str| headers.get(name).and_then(|value| value.to_str().ok());
    verifier
        .verify(&WebhookHeaders::from_lookup(lookup).unwrap(), raw)
        .expect("the delivery verifies with the subscription's secret");
    assert_eq!(headers["webhook-id"], body["id"].as_str().unwrap());
    assert_eq!(headers["awp-version"], "2.0");
}

// --- subscriptions -------------------------------------------------------------------------

/// Subscriptions need a verified requester, belong to it, and receive signed Standard
/// Webhooks deliveries of the types they name (AWP 2.0 §9.1–§9.3).
#[tokio::test]
async fn subscriptions_are_owned_and_deliveries_are_signed() {
    let receiver = Receiver::start().await;
    let site = site(local_events()).await;

    let (status, _, body) = site
        .send(request(
            "POST",
            "/awp/v2/events",
            Some(json!({ "types": ["catalog.changed"], "url": receiver.url })),
        ))
        .await;
    assert_eq!(
        (status, &body["error"]["code"]),
        (StatusCode::UNAUTHORIZED, &json!("unauthorized"))
    );
    let (status, _, body) = site
        .as_buyer(request(
            "POST",
            "/awp/v2/events",
            Some(json!({ "types": ["no.such.event"], "url": receiver.url })),
        ))
        .await;
    assert_eq!(
        (status, &body["error"]["code"]),
        (StatusCode::BAD_REQUEST, &json!("invalid_request"))
    );

    let subscription =
        site.subscribe(&receiver.url, &["site.status.changed", "order.updated"]).await;
    let id = subscription["id"].as_str().unwrap().to_owned();
    assert!(subscription["secret"].as_str().unwrap().starts_with("whsec_"));
    let (status, _, read) =
        site.as_buyer(request("GET", &format!("/awp/v2/events/{id}"), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(read.get("secret").is_none(), "the secret is shown once");
    let mut stranger = request("GET", &format!("/awp/v2/events/{id}"), None);
    site.other.sign_request(&mut stranger).unwrap();
    assert_eq!(site.send(stranger).await.0, StatusCode::NOT_FOUND);

    // A status transition is published, signed, with AWP-Version.
    site.edge.set_mode(EdgeMode::read_only("drift_detected")).await.unwrap();
    let hits = receiver.wait_for(1).await;
    let (headers, body) = &hits[0];
    assert_eq!(body["type"], "site.status.changed");
    assert_eq!(body["data"]["status"]["state"], "read_only");
    assert_eq!(body["data"]["status"]["reasonCode"], "drift_detected");
    let raw = receiver.hits.lock().unwrap()[0].1.clone();
    verify_delivery(&verifier(&subscription), headers, &raw, body);

    // order.updated reaches only the requester that placed the order.
    let other = RequesterId::web_bot_auth(OTHER, "k");
    site.edge
        .events()
        .order_updated(
            &other,
            OrderUpdated { order_id: "o-1".into(), state: "paid".into(), receipt: None },
        )
        .await
        .unwrap();
    site.edge
        .events()
        .order_updated(
            &site.buyer_id,
            OrderUpdated { order_id: "o-2".into(), state: "paid".into(), receipt: None },
        )
        .await
        .unwrap();
    let hits = receiver.wait_for(2).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let orders: Vec<&Value> =
        hits.iter().map(|(_, body)| body).filter(|body| body["type"] == "order.updated").collect();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0]["data"]["orderId"], "o-2");

    let (status, _, changed) = site
        .as_buyer(request(
            "PATCH",
            &format!("/awp/v2/events/{id}"),
            Some(json!({ "types": ["catalog.changed"] })),
        ))
        .await;
    assert_eq!((status, &changed["types"]), (StatusCode::OK, &json!(["catalog.changed"])));
    assert_eq!(
        site.as_buyer(request("DELETE", &format!("/awp/v2/events/{id}"), None)).await.0,
        StatusCode::NO_CONTENT
    );
}

/// Delivery URLs must be public `https` by default (AWP 2.0 §9.1, §19.11).
#[tokio::test]
async fn private_and_plain_http_urls_are_refused() {
    let site = site(EventsConfig::default()).await;
    for url in [
        "http://hooks.example.com/a",
        "https://127.0.0.1/a",
        "https://10.0.0.8/a",
        "https://localhost/a",
    ] {
        let (status, _, body) = site
            .as_buyer(request(
                "POST",
                "/awp/v2/events",
                Some(json!({ "types": ["catalog.changed"], "url": url })),
            ))
            .await;
        assert_eq!(
            (status, &body["error"]["code"]),
            (StatusCode::BAD_REQUEST, &json!("invalid_request")),
            "{url}"
        );
    }
}

/// Six consecutive failed deliveries suspend a subscription, which receives one final
/// `subscription.suspended`; reactivation resumes it (AWP 2.0 §9.4).
#[tokio::test]
async fn six_failures_suspend_with_a_final_notice() {
    let receiver = Receiver::start().await;
    receiver.status.store(500, Ordering::SeqCst);
    let site = site(local_events()).await;
    let subscription = site.subscribe(&receiver.url, &["site.release.published"]).await;
    let id = subscription["id"].as_str().unwrap().to_owned();
    site.edge
        .events()
        .release_published(awp_types::ReleasePublished {
            seq: 42,
            manifest: "https://example.com/.well-known/awp/releases/42.json".into(),
            digest: Digest::from_sha256_bytes([4; 32]),
            base: None,
        })
        .await
        .unwrap();
    let hits = receiver.wait_for(7).await;
    let kinds: Vec<&str> = hits.iter().map(|(_, body)| body["type"].as_str().unwrap()).collect();
    assert_eq!(kinds[..6], ["site.release.published"; 6]);
    assert_eq!(kinds[6], "subscription.suspended");
    assert_eq!(hits[6].1["data"]["failures"], 6);
    let ids: std::collections::BTreeSet<&str> =
        hits[..6].iter().map(|(headers, _)| headers["webhook-id"].to_str().unwrap()).collect();
    assert_eq!(ids.len(), 1, "every retry carries the event's id");
    let (_, _, state) = site.as_buyer(request("GET", &format!("/awp/v2/events/{id}"), None)).await;
    assert_eq!((state["state"].as_str(), state["failures"].as_u64()), (Some("suspended"), Some(6)));

    receiver.status.store(200, Ordering::SeqCst);
    let (_, _, active) = site
        .as_buyer(request(
            "PATCH",
            &format!("/awp/v2/events/{id}"),
            Some(json!({ "state": "active" })),
        ))
        .await;
    assert_eq!((active["state"].as_str(), active["failures"].as_u64()), (Some("active"), Some(0)));
}

/// Replay redelivers retained events under their original ids (AWP 2.0 §9.5); a release's
/// `catalog.changed` carries 1,000 ids and pages the rest (§9.7).
#[tokio::test]
async fn replay_and_catalogue_pages() {
    let receiver = Receiver::start().await;
    let since = Utc::now() - chrono::Duration::seconds(1);
    let site = site(local_events()).await;
    let subscription = site.subscribe(&receiver.url, &["catalog.changed"]).await;
    let items: Vec<ContentId> = (0..2_500).map(|n| ContentId(format!("sku-{n}"))).collect();
    site.edge.events().catalog_changed(7, items).await.unwrap();
    let first = receiver.wait_for(1).await;
    let data = &first[0].1["data"];
    assert_eq!(
        (data["items"].as_array().unwrap().len(), data["total"].as_u64()),
        (1_000, Some(2_500))
    );
    let next = data["next"].as_str().unwrap().to_owned();
    assert_eq!(next, format!("{EDGE}/awp/v2/events/changes/7/2"), "adk-site's page URLs");
    let path = next.trim_start_matches(EDGE).to_owned();
    let (status, _, page) = site.as_buyer(request("GET", &path, None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["items"][0], "sku-1000");
    let (_, _, last) = site
        .as_buyer(request("GET", page["next"].as_str().unwrap().trim_start_matches(EDGE), None))
        .await;
    assert_eq!(last["items"].as_array().unwrap().len(), 500);
    assert!(last.get("next").is_none());
    assert_eq!(site.send(request("GET", &path, None)).await.0, StatusCode::UNAUTHORIZED);

    let replay = json!({ "subscription": subscription["id"], "since": since });
    let (status, _, accepted) =
        site.as_buyer(request("POST", "/awp/v2/events/replay", Some(replay))).await;
    assert_eq!((status, &accepted["accepted"]), (StatusCode::ACCEPTED, &json!(1)));
    let hits = receiver.wait_for(2).await;
    assert_eq!(hits[1].0["webhook-id"], hits[0].0["webhook-id"], "replays keep the original id");
}

// --- consent -------------------------------------------------------------------------------

/// Consent is granted as a signed receipt to a pseudonymous subject, exported and erased by
/// the subject alone; a revocation stops the pending call that relied on it and tells the
/// requester that referenced it (A-I8; AWP 2.0 §11).
#[tokio::test]
async fn consent_receipts_revocation_export_and_erasure() {
    let receiver = Receiver::start().await;
    let site = site(local_events()).await;
    let grant = json!({ "scope": "proactive_outreach", "policyVersion": "2026-10", "jurisdiction": "ke", "channel": "web" });
    let (status, headers, receipt) =
        site.send(request("POST", "/awp/v2/consent", Some(grant))).await;
    assert_eq!(status, StatusCode::CREATED, "{receipt}");
    let token = headers["awp-subject"].to_str().unwrap().to_owned();
    let parsed: ConsentReceipt = serde_json::from_value(receipt.clone()).unwrap();
    assert_eq!(parsed.jurisdiction, "KE");
    assert!(parsed.subject_ref.starts_with("v:"), "the subject is a random visitor id");
    verify(&parsed, &parsed.proof, &site.receipt_key.public_key())
        .expect("the receipt is signed by the receipt key");
    for bad in [
        json!({ "scope": "payment_data", "policyVersion": "2026-10", "jurisdiction": "KE" }),
        json!({ "scope": "newsletter", "policyVersion": "2025-01", "jurisdiction": "KE" }),
    ] {
        assert_eq!(
            site.send(request("POST", "/awp/v2/consent", Some(bad))).await.0,
            StatusCode::BAD_REQUEST
        );
    }

    // The buyer's agent asks for a call back under the visitor's consent; the owner decides.
    site.subscribe(&receiver.url, &["consent.revoked"]).await;
    let mut call = request("POST", "/awp/v2/capabilities/request_callback", Some(json!({})));
    call.headers_mut().insert("awp-consent", parsed.id.to_string().parse().unwrap());
    let (status, headers, held) = site.as_buyer(call).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{held}");
    let location = headers[header::LOCATION].to_str().unwrap().to_owned();

    // No session, no access: the requester's tier never reaches a subject's receipts.
    let (status, _, _) = site.as_buyer(request("GET", "/awp/v2/consent/export", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let mut export = request("GET", &format!("/awp/v2/consent/export?subject={token}"), None);
    export.headers_mut().remove("awp-subject");
    let (status, _, exported) = site.send(export).await;
    assert_eq!(status, StatusCode::OK, "a signed link authenticates the subject");
    assert_eq!(exported["subjectRef"], parsed.subject_ref.as_str());
    assert_eq!(exported["receipts"][0]["receipt"]["id"], parsed.id.to_string());

    let mut revoke =
        request("POST", "/awp/v2/consent/revoke", Some(json!({ "receipt": parsed.id })));
    revoke.headers_mut().insert("awp-subject", token.parse().unwrap());
    let (status, _, revoked) = site.send(revoke).await;
    assert_eq!((status, &revoked["revoked"][0]), (StatusCode::ACCEPTED, &json!(parsed.id)));
    let (_, _, stopped) = site.as_buyer(request("GET", &location, None)).await;
    assert_eq!(stopped["state"], "rejected");
    assert_eq!(stopped["error"]["code"], "consent_required");
    let hits = receiver.wait_for(1).await;
    assert_eq!(hits[0].1["type"], "consent.revoked");
    assert_eq!(hits[0].1["data"]["receipt"], parsed.id.to_string());
    let mut again =
        request("POST", "/awp/v2/capabilities/request_callback", Some(json!({ "again": true })));
    again.headers_mut().insert("awp-consent", parsed.id.to_string().parse().unwrap());
    assert_eq!(site.as_buyer(again).await.2["error"]["code"], "consent_required");

    let mut erase = request("POST", "/awp/v2/consent/erase", None);
    erase.headers_mut().insert("awp-subject", token.parse().unwrap());
    let (status, _, erased) = site.send(erase).await;
    assert_eq!((status, &erased["erased"]["receipts"]), (StatusCode::ACCEPTED, &json!(1)));
    let mut export = request("GET", "/awp/v2/consent/export", None);
    export.headers_mut().insert("awp-subject", token.parse().unwrap());
    assert_eq!(site.send(export).await.2["receipts"], json!([]));
}

/// Erasure reaches the leads that reference the subject's receipts and leaves the others
/// (spec 3.5 §7.5; AWP 2.0 §11.3).
#[tokio::test]
async fn erasure_deletes_the_subjects_leads() {
    let stores = MemoryEdgeStores::default();
    let site = site_over(local_events(), &stores).await;
    let grant = json!({ "scope": "newsletter", "policyVersion": "2026-10", "jurisdiction": "KE" });
    let (status, headers, receipt) =
        site.send(request("POST", "/awp/v2/consent", Some(grant))).await;
    assert_eq!(status, StatusCode::CREATED, "{receipt}");
    let token = headers["awp-subject"].to_str().unwrap().to_owned();
    let receipt: ConsentReceipt = serde_json::from_value(receipt).unwrap();
    let lead = |id: &str, consent: uuid::Uuid| Lead {
        id: LeadRef(id.to_owned()),
        captured_at: Utc::now(),
        source: Channel::Web,
        contact_ref: ContactRef(format!("contact-{id}")),
        consent: vec![ConsentReceiptRef(consent)],
        answers: BTreeMap::new(),
        thread_ref: None,
        score: None,
    };
    stores.leads.insert(lead("theirs", receipt.id)).await.unwrap();
    stores.leads.insert(lead("someone-else", uuid::Uuid::new_v4())).await.unwrap();

    let mut export = request("GET", "/awp/v2/consent/export", None);
    export.headers_mut().insert("awp-subject", token.parse().unwrap());
    let (_, _, exported) = site.send(export).await;
    assert_eq!(exported["data"]["leads"][0]["id"], "theirs");

    let mut erase = request("POST", "/awp/v2/consent/erase", None);
    erase.headers_mut().insert("awp-subject", token.parse().unwrap());
    let (status, _, erased) = site.send(erase).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(erased["erased"], json!({ "receipts": 1, "data": 1 }));
    assert_eq!(stores.leads.get(&LeadRef("theirs".to_owned())).await.unwrap(), None);
    assert!(stores.leads.get(&LeadRef("someone-else".to_owned())).await.unwrap().is_some());
}

/// The site reads back what its edge announced, oldest first and within the window asked
/// for, with each event's audience (spec 3.5 §7.5).
#[tokio::test]
async fn the_site_reads_its_retained_events() {
    let site = site(local_events()).await;
    let before = Utc::now() - chrono::Duration::seconds(1);
    let events = site.edge.events();
    let catalog = events.catalog_changed(3, vec![ContentId("sku-1".to_owned())]).await.unwrap();
    let order = events
        .order_updated(
            &site.buyer_id,
            OrderUpdated { order_id: "o-1".to_owned(), state: "paid".to_owned(), receipt: None },
        )
        .await
        .unwrap();
    let retained = events.retained(before, None).await.unwrap();
    assert_eq!(
        retained
            .iter()
            .map(|r| (r.event.id.as_str(), r.event.event_type.clone()))
            .collect::<Vec<_>>(),
        vec![
            (catalog.as_str(), EventType::CatalogChanged),
            (order.as_str(), EventType::OrderUpdated)
        ]
    );
    assert_eq!(retained[0].audience, None);
    assert_eq!(
        retained[1].audience,
        Some(std::collections::BTreeSet::from([site.buyer_id.clone()]))
    );
    assert!(
        events.retained(Utc::now() + chrono::Duration::seconds(5), None).await.unwrap().is_empty()
    );
    assert!(events.retained(before, Some(before)).await.unwrap().is_empty());
}

// --- status --------------------------------------------------------------------------------

/// The kill switch publishes a signed `read_only` status with reason `kill_switch`, and its
/// lifting an `operational` one; each transition is an event (spec 3.5 §7.5, A-I12).
#[tokio::test]
async fn status_transitions_are_signed_and_published() {
    let receiver = Receiver::start().await;
    let site = site(local_events()).await;
    let control = GovernanceControl::new();
    site.edge.attach_governance(control.clone());
    site.subscribe(&receiver.url, &["site.status.changed"]).await;
    control.freeze("incident 9");
    let (_, _, status) = site.send(request("GET", "/awp/v2/status", None)).await;
    assert_eq!(
        (status["state"].as_str(), status["reasonCode"].as_str()),
        (Some("read_only"), Some("kill_switch"))
    );
    let document: awp_types::SiteStatus = serde_json::from_value(status).unwrap();
    verify(&document, &document.proof, &site.receipt_key.public_key()).unwrap();
    control.unfreeze();
    let (_, _, status) = site.send(request("GET", "/awp/v2/status", None)).await;
    assert_eq!(status["state"], "operational");
    let hits = receiver.wait_for(2).await;
    let states: Vec<&str> =
        hits.iter().map(|(_, body)| body["data"]["status"]["state"].as_str().unwrap()).collect();
    assert_eq!(states, ["read_only", "operational"]);
    assert_eq!(hits[1].1["data"]["previous"], "read_only");
}

// --- beacon --------------------------------------------------------------------------------

/// The beacon counts page views and events per day, sets no cookie, keeps paths free of
/// queries, counts nothing under Global Privacy Control, and identifies a visitor only under
/// a live `persistent_profile` receipt (spec 3.5 §7.12).
#[tokio::test]
async fn the_beacon_counts_without_identifiers() {
    let site = site(EventsConfig::default()).await;
    let beacon = |body: Value, gpc: bool| {
        let mut request = Request::post(format!("{EDGE}/awp/v2/beacon"))
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from(body.to_string()))
            .unwrap();
        if gpc {
            request.headers_mut().insert("sec-gpc", "1".parse().unwrap());
        }
        request
    };
    let (status, headers, _) = site
        .send(beacon(
            json!({ "kind": "pageview", "path": "/pricing?email=ana@example.com" }),
            false,
        ))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(headers.get(header::SET_COOKIE).is_none());
    site.send(beacon(json!({ "kind": "pageview", "path": "/pricing" }), true)).await;
    site.send(beacon(json!({ "kind": "event", "name": "signup" }), false)).await;
    assert_eq!(
        site.send(beacon(json!({ "kind": "event", "name": "Sign Up" }), false)).await.0,
        StatusCode::BAD_REQUEST
    );

    let grant =
        json!({ "scope": "persistent_profile", "policyVersion": "2026-10", "jurisdiction": "KE" });
    let (_, _, receipt) = site.send(request("POST", "/awp/v2/consent", Some(grant))).await;
    site.send(beacon(json!({ "kind": "pageview", "path": "/", "receipt": receipt["id"] }), false))
        .await;
    site.send(beacon(json!({ "kind": "pageview", "path": "/", "receipt": receipt["id"] }), false))
        .await;
    site.send(beacon(
        json!({ "kind": "pageview", "path": "/", "receipt": uuid::Uuid::new_v4() }),
        false,
    ))
    .await;

    let counts = site.edge.beacon();
    let today = Utc::now();
    let tomorrow = today + chrono::Duration::days(1);
    let read = |metric: &'static str| {
        let counts = counts.clone();
        async move { counts.read(metric, today, tomorrow).await.unwrap() }
    };
    assert_eq!(read("pageviews").await, Some(4.0), "the GPC view counts nothing");
    assert_eq!(read("pageviews:/pricing").await, Some(1.0));
    assert_eq!(read("events:signup").await, Some(1.0));
    assert_eq!(read("visitors").await, Some(1.0), "one consenting visitor, counted once");
}

// --- the event log and beacon store contract -----------------------------------------------

mod memory_log {
    adk_server::edge_log_conformance_tests! {
        harness = adk_server::awp::conformance::MemoryEdgeLog::default(),
    }
}

mod memory_rate_limits {
    adk_server::rate_limit_conformance_tests! {
        harness = adk_server::awp::conformance::MemoryRateLimits::default(),
    }
}
