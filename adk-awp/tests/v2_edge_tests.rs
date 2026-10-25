//! The AWP 2.0 edge routes end to end over an in-process router (spec 3.5 §7.2–§7.4, §7.9,
//! A-I6, A-I7, A-I8).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_awp::v2::capability::{
    CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry,
};
use adk_awp::v2::executor::{DirectExecutor, HeldDecision};
use adk_awp::v2::routes::{EdgeState, awp_v2_routes};
use adk_awp::v2::store::{ConsentReceiptStore, HeldCallStore, MemoryEdgeStores, StatusStore};
use adk_awp::v2::tier::{TierAssigner, TierAssignment};
use async_trait::async_trait;
use awp_types::{
    ApprovalHint, CapabilityEffect, CapabilityManifestV2, CapabilityV2, Channel, ConsentReceipt,
    ConsentRequirement, ConsentScope, Deprecation, Digest, ErrorCode, ErrorEnvelope, HeldCall,
    HeldCallState, LegalBasis, Proof, RequesterId, SiteState, SiteStatus, TrustRule, TrustRules,
    TrustTier,
};
use axum::Router;
use axum::body::Body;
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use proptest::prelude::*;
use serde_json::{Value, json};
use tower::util::ServiceExt;
use uuid::Uuid;

/// Test-only assigner: trusts `x-test-tier` and `x-test-requester`. Real assigners verify a
/// credential.
struct HeaderTiers;

#[async_trait]
impl TierAssigner for HeaderTiers {
    async fn assign(&self, head: &Parts) -> TierAssignment {
        let tier = head
            .headers
            .get("x-test-tier")
            .and_then(|value| serde_json::from_value(json!(value.to_str().unwrap())).ok())
            .unwrap_or(TrustTier::Public);
        let requester = head
            .headers
            .get("x-test-requester")
            .map(|value| RequesterId::new(value.to_str().unwrap()));
        TierAssignment { tier, requester }
    }
}

#[derive(Default)]
struct Counting {
    calls: AtomicUsize,
}

#[async_trait]
impl CapabilityHandler for Counting {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if call.input.get("sku").and_then(Value::as_str) == Some("SOLD-OUT") {
            return Err(CapabilityError::refused(ErrorCode::InventoryUnavailable, "sold out"));
        }
        Ok(json!({
            "call": n,
            "input": call.input,
            "requester": call.requester.map(|r| r.to_string()),
            "tier": call.tier,
            "key": call.idempotency_key,
        }))
    }
}

struct Edge {
    app: Router,
    state: EdgeState,
    stores: MemoryEdgeStores,
    executor: Arc<DirectExecutor>,
    orders: Arc<Counting>,
    bookings: Arc<Counting>,
}

fn proof() -> Proof {
    Proof::eddsa("edge-2026-10", "e30..c2ln")
}

fn edge() -> Edge {
    let orders = Arc::new(Counting::default());
    let bookings = Arc::new(Counting::default());
    let search = Arc::new(Counting::default());
    let mut registry = CapabilityRegistry::new("example.com");
    registry
        .declare(
            CapabilityV2::new("search_site", "Search", CapabilityEffect::ReadOnly),
            search.clone(),
        )
        .unwrap();
    let mut place_order =
        CapabilityV2::new("place_order", "Order", CapabilityEffect::NonIdempotent)
            .with_min_trust(TrustTier::Known);
    place_order.input_schema = Some(json!({
        "type": "object",
        "required": ["sku"],
        "properties": { "sku": { "type": "string" }, "amount": { "type": "integer" } }
    }));
    place_order.approval = ApprovalHint::AboveAmount(100_000);
    registry.declare(place_order, orders.clone()).unwrap();
    let book = CapabilityV2 {
        consent: Some(ConsentRequirement {
            scope: ConsentScope::PersistentProfile,
            policy_version: "2026-10".to_owned(),
        }),
        ..CapabilityV2::new("book", "Book", CapabilityEffect::Idempotent)
    };
    registry.declare(book, bookings.clone()).unwrap();
    registry
        .declare(
            CapabilityV2::new("wholesale", "Partners only", CapabilityEffect::ReadOnly)
                .with_min_trust(TrustTier::Partner),
            search.clone(),
        )
        .unwrap();
    registry
        .declare(
            CapabilityV2 {
                deprecated: Some(Deprecation {
                    successor: Some("search_site".to_owned()),
                    sunset: Some(Utc::now() - Duration::days(1)),
                }),
                ..CapabilityV2::new("old_search", "Retired", CapabilityEffect::ReadOnly)
            },
            search,
        )
        .unwrap();

    let stores = MemoryEdgeStores::default();
    let registry = Arc::new(registry);
    let executor = Arc::new(DirectExecutor::in_memory(Arc::clone(&registry), &stores));
    let state = EdgeState::builder((*registry).clone())
        .executor(executor.clone())
        .tiers(Arc::new(HeaderTiers))
        .memory_stores(stores.clone())
        .trust_rules(TrustRules {
            accepts: vec![],
            rules: vec![TrustRule {
                subject: "wba:https://partner.example#*".to_owned(),
                tier: TrustTier::Partner,
                rate_class: None,
                note: None,
            }],
            rate_classes: Default::default(),
        })
        .build();
    Edge { app: awp_v2_routes(state.clone()), state, stores, executor, orders, bookings }
}

async fn send(app: &Router, request: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let body = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, headers, body)
}

fn get(path: &str) -> axum::http::request::Builder {
    Request::get(path)
}

fn call(name: &str) -> axum::http::request::Builder {
    Request::post(format!("/awp/v2/capabilities/{name}")).header("content-type", "application/json")
}

fn known(builder: axum::http::request::Builder, requester: &str) -> axum::http::request::Builder {
    builder.header("x-test-tier", "known").header("x-test-requester", requester)
}

/// Validates `document` against a committed AWP 2.0 schema (`awp-types/schemas/2.0/`).
fn assert_schema(stem: &str, document: &Value) {
    let path =
        format!("{}/../awp-types/schemas/2.0/{stem}.schema.json", env!("CARGO_MANIFEST_DIR"));
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<String> = validator.iter_errors(document).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{stem} schema: {errors:?}\n{document}");
}

#[tokio::test]
async fn manifest_lists_only_what_the_tier_may_see() {
    let edge = edge();
    let (status, headers, body) =
        send(&edge.app, get("/awp/v2/manifest").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/awp+json");
    assert_eq!(headers["awp-version"], "2.0");
    assert_eq!(headers["vary"], "AWP-Version");
    assert_schema("capability-manifest", &body);
    let manifest: CapabilityManifestV2 = serde_json::from_value(body).unwrap();
    assert_eq!(manifest.tier, Some(TrustTier::Public));
    let names: Vec<&str> = manifest.capabilities.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["book", "old_search", "search_site"]);

    let (_, _, body) = send(
        &edge.app,
        get("/awp/v2/manifest").header("x-test-tier", "partner").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(body["capabilities"].as_array().unwrap().len(), 5);
}

/// A capability above the caller's tier answers exactly as one that does not exist
/// (AWP 2.0 §7.3).
#[tokio::test]
async fn hidden_and_missing_capabilities_answer_alike() {
    let edge = edge();
    let hidden = send(&edge.app, call("place_order").body(Body::from("{}")).unwrap()).await;
    let missing = send(&edge.app, call("no_such").body(Body::from("{}")).unwrap()).await;
    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    assert_eq!(hidden.0, missing.0);
    assert_eq!(hidden.2, missing.2);
    assert_eq!(hidden.2["error"]["code"], "capability_not_found");
    assert_schema("error-envelope", &hidden.2);
    let retired = send(&edge.app, call("old_search").body(Body::from("{}")).unwrap()).await;
    assert_eq!(retired.2, missing.2, "a capability past its sunset is gone");
}

/// A `NonIdempotent` capability executes at most once per idempotency key (A-I7).
#[tokio::test]
async fn non_idempotent_calls_execute_once_per_key() {
    let edge = edge();
    let buyer = "wba:https://buyer.example#k1";
    let order = || known(call("place_order"), buyer);

    let (status, _, body) =
        send(&edge.app, order().body(Body::from(r#"{"sku":"PANEL-400"}"#)).unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "idempotency_key_required");

    let first = send(
        &edge.app,
        order()
            .header("Idempotency-Key", "\"order-1\"")
            .body(Body::from(r#"{"sku":"PANEL-400"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(first.1["content-type"], "application/json");
    assert_eq!(first.2["requester"], buyer);
    assert_eq!(first.2["key"], "order-1");
    let repeat = send(
        &edge.app,
        order()
            .header("Idempotency-Key", "order-1")
            .body(Body::from(r#"{ "sku": "PANEL-400" }"#))
            .unwrap(),
    )
    .await;
    assert_eq!(repeat.0, StatusCode::OK);
    assert_eq!(repeat.2, first.2, "the recorded outcome is returned");
    assert_eq!(edge.orders.calls.load(Ordering::SeqCst), 1);

    let conflict = send(
        &edge.app,
        order()
            .header("Idempotency-Key", "order-1")
            .body(Body::from(r#"{"sku":"INVERTER"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(conflict.0, StatusCode::CONFLICT);
    assert_eq!(conflict.2["error"]["code"], "idempotency_conflict");
    assert_eq!(conflict.2["error"]["retryable"], false);

    // The key is scoped to the requester: another buyer's identical key is a new call.
    let other = send(
        &edge.app,
        known(call("place_order"), "wba:https://other.example#k2")
            .header("Idempotency-Key", "order-1")
            .body(Body::from(r#"{"sku":"PANEL-400"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(other.0, StatusCode::OK);
    assert_eq!(edge.orders.calls.load(Ordering::SeqCst), 2);

    // A refusal from the handler is recorded and replayed too.
    let sold_out = || {
        order()
            .header("Idempotency-Key", "order-2")
            .body(Body::from(r#"{"sku":"SOLD-OUT"}"#))
            .unwrap()
    };
    let refused = send(&edge.app, sold_out()).await;
    assert_eq!(refused.0, StatusCode::CONFLICT);
    assert_eq!(refused.2["error"]["code"], "inventory_unavailable");
    assert_eq!(refused.2["error"]["retryable"], true);
    assert_eq!(send(&edge.app, sold_out()).await.2, refused.2);
    assert_eq!(edge.orders.calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn invalid_input_is_refused_before_anything_runs() {
    let edge = edge();
    let buyer = "wba:https://buyer.example#k1";
    let (status, _, body) = send(
        &edge.app,
        known(call("place_order"), buyer)
            .header("Idempotency-Key", "k")
            .body(Body::from(r#"{"sku": 7}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "schema_validation_failed");
    let (status, _, body) =
        send(&edge.app, call("search_site").body(Body::from("not json")).unwrap()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
    let (status, _, _) = send(
        &edge.app,
        call("search_site").header("AWP-Consent", "\"not-a-uuid\"").body(Body::from("{}")).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(edge.orders.calls.load(Ordering::SeqCst), 0);
}

/// A call above the approval threshold answers 202 with its status URL; the owner's
/// decision runs it; only its requester reads it (spec 3.5 §7.4).
#[tokio::test]
async fn held_calls_answer_202_and_resolve_on_decision() {
    let edge = edge();
    let buyer = "wba:https://buyer.example#k1";
    let big = || {
        known(call("place_order"), buyer)
            .header("Idempotency-Key", "big-1")
            .body(Body::from(r#"{"sku":"ARRAY","amount":250000}"#))
            .unwrap()
    };
    let (status, headers, body) = send(&edge.app, big()).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_schema("held-call", &body);
    let held: HeldCall = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(held.state, HeldCallState::Pending);
    assert_eq!(headers["location"], held.status.as_str());
    assert!(held.id.len() >= 32, "an unguessable id");
    assert_eq!(edge.orders.calls.load(Ordering::SeqCst), 0, "nothing runs before the decision");

    let (status, _, repeat) = send(&edge.app, big()).await;
    assert_eq!((status, repeat), (StatusCode::ACCEPTED, body), "a repeat returns the same call");

    let (status, headers, polled) =
        send(&edge.app, known(get(&held.status), buyer).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(polled["state"], "pending");
    assert_eq!(headers["retry-after"], "30");
    let (status, _, _) = send(
        &edge.app,
        known(get(&held.status), "wba:https://other.example#k9").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another requester cannot read it");

    let decided = edge.executor.decide(&held.id, HeldDecision::Approve).await.unwrap();
    assert_eq!(decided.state, HeldCallState::Completed);
    assert_eq!(decided.result.as_ref().unwrap()["input"]["sku"], "ARRAY");
    let (_, _, polled) =
        send(&edge.app, known(get(&held.status), buyer).body(Body::empty()).unwrap()).await;
    assert_schema("held-call", &polled);
    assert_eq!(polled["state"], "completed");
    assert_eq!(edge.orders.calls.load(Ordering::SeqCst), 1);
    assert!(edge.executor.decide(&held.id, HeldDecision::Approve).await.is_err());
}

#[tokio::test]
async fn rejected_and_expired_calls_do_not_run() {
    let edge = edge();
    let buyer = "wba:https://buyer.example#k1";
    let hold = |key: &str| {
        known(call("place_order"), buyer)
            .header("Idempotency-Key", key)
            .body(Body::from(r#"{"sku":"ARRAY","amount":250000}"#))
            .unwrap()
    };
    let (_, _, first) = send(&edge.app, hold("a")).await;
    let rejected = edge
        .executor
        .decide(first["id"].as_str().unwrap(), HeldDecision::Reject { message: "too large".into() })
        .await
        .unwrap();
    assert_eq!(rejected.state, HeldCallState::Rejected);
    assert_eq!(rejected.error.unwrap().message, "too large");

    // An executor configured with a hold expiry gives pending calls an `expiresAt`.
    let expiring = MemoryEdgeStores::default();
    let executor = DirectExecutor::in_memory(Arc::clone(edge.state.registry()), &expiring)
        .with_hold_expiry(Duration::minutes(10));
    let invocation = adk_awp::v2::executor::Invocation {
        tier: TrustTier::Known,
        requester: Some(RequesterId::new(buyer)),
        idempotency_key: Some("c".to_owned()),
        ..adk_awp::v2::executor::Invocation::new(
            "place_order",
            json!({ "sku": "ARRAY", "amount": 250_000 }),
            adk_awp::v2::executor::Binding::Http,
        )
    };
    let adk_awp::v2::executor::InvocationOutcome::Held(call) =
        adk_awp::v2::executor::CapabilityExecutor::invoke(&executor, invocation).await
    else {
        panic!("the call is held");
    };
    let expires_at = call.expires_at.expect("an expiry is set");
    assert!(executor.expire_due(expires_at - Duration::seconds(1)).await.unwrap().is_empty());
    assert_eq!(executor.expire_due(expires_at).await.unwrap(), vec![call.id.clone()]);
    let expired = HeldCallStore::get(&expiring.held_calls, &call.id).await.unwrap().unwrap();
    assert_eq!(expired.call.state, HeldCallState::Expired);
    assert!(expired.call.error.is_some());
    assert_eq!(edge.orders.calls.load(Ordering::SeqCst), 0);
}

/// A consent-bound call refuses without a live receipt for its scope and policy version
/// (A-I8).
#[tokio::test]
async fn consent_bound_calls_need_a_live_receipt() {
    let edge = edge();
    let (status, _, body) = send(&edge.app, call("book").body(Body::from("{}")).unwrap()).await;
    assert_eq!(status.as_u16(), 451);
    assert_eq!(body["error"]["code"], "consent_required");

    let receipt = ConsentReceipt {
        id: Uuid::now_v7(),
        subject_ref: "subj_1".to_owned(),
        scope: ConsentScope::PersistentProfile,
        policy_version: "2026-10".to_owned(),
        policy_digest: Digest::from_sha256_bytes([1; 32]),
        legal_basis: LegalBasis::ExplicitConsent,
        jurisdiction: "KE".to_owned(),
        granted_at: Utc::now() - Duration::minutes(5),
        expires_at: None,
        channel: Channel::Web,
        proof: proof(),
    };
    edge.stores.consent.insert(receipt.clone()).await.unwrap();
    let with_receipt = || {
        call("book")
            .header("AWP-Consent", format!("\"{}\", \"{}\"", Uuid::now_v7(), receipt.id))
            .body(Body::from("{}"))
            .unwrap()
    };
    let (status, _, _) = send(&edge.app, with_receipt()).await;
    assert_eq!(status, StatusCode::OK);
    edge.stores.consent.revoke(receipt.id, Utc::now()).await.unwrap();
    let (status, _, _) = send(&edge.app, with_receipt()).await;
    assert_eq!(status.as_u16(), 451, "a revoked receipt is not live");
    assert_eq!(edge.bookings.calls.load(Ordering::SeqCst), 1);
}

/// In read-only mode only `read_only` capabilities answer; the rest get 503 with the status
/// document (spec 3.5 §7.9, A-I11).
#[tokio::test]
async fn read_only_mode_answers_503_with_the_status() {
    let edge = edge();
    let mut status = SiteStatus::new(SiteState::ReadOnly, Utc::now(), proof());
    status.reason_code = Some("drift_detected".into());
    status.release_seq = Some(42);
    edge.stores.status.set(status.clone()).await.unwrap();

    let (code, headers, body) = send(&edge.app, call("book").body(Body::from("{}")).unwrap()).await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_schema("error-envelope", &body);
    let envelope: ErrorEnvelope = serde_json::from_value(body).unwrap();
    assert_eq!(envelope.error.code, ErrorCode::ReadOnly);
    assert_eq!(envelope.status, Some(status.clone()));
    assert_eq!(headers["content-type"], "application/awp+json");
    let (code, _, _) = send(&edge.app, call("search_site").body(Body::from("{}")).unwrap()).await;
    assert_eq!(code, StatusCode::OK, "read-only capabilities still answer");

    let (code, _, served) =
        send(&edge.app, get("/awp/v2/status").body(Body::empty()).unwrap()).await;
    assert_eq!(code, StatusCode::OK);
    assert_schema("site-status", &served);
    assert_eq!(serde_json::from_value::<SiteStatus>(served).unwrap(), status);

    edge.stores
        .status
        .set(SiteStatus::new(SiteState::Operational, Utc::now(), proof()))
        .await
        .unwrap();
    edge.state.set_read_only(true);
    let (code, _, body) = send(&edge.app, call("book").body(Body::from("{}")).unwrap()).await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["status"]["state"], "operational");
    edge.state.set_read_only(false);
}

#[tokio::test]
async fn status_and_trust_rules_are_published() {
    let edge = edge();
    let (code, _, body) = send(&edge.app, get("/awp/v2/status").body(Body::empty()).unwrap()).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "resource_not_found");
    let (code, _, body) = send(&edge.app, get("/awp/v2/trust").body(Body::empty()).unwrap()).await;
    assert_eq!(code, StatusCode::OK);
    assert_schema("trust-rules", &body);
    assert_eq!(body["rules"][0]["tier"], "partner");
}

/// `AWP-Version` selects the highest common major (spec 3.5 §7.2; AWP 2.0 §18.2).
#[tokio::test]
async fn version_negotiation_matrix() {
    let edge = edge();
    for (offered, expected) in [
        (None, StatusCode::OK),
        (Some("2.0"), StatusCode::OK),
        (Some("2.0, 1.0"), StatusCode::OK),
        (Some("1.0, 2.1"), StatusCode::OK),
        (Some("1.0"), StatusCode::NOT_ACCEPTABLE),
        (Some("3.0"), StatusCode::NOT_ACCEPTABLE),
        (Some("two"), StatusCode::BAD_REQUEST),
    ] {
        let mut request = get("/awp/v2/manifest");
        if let Some(offered) = offered {
            request = request.header("AWP-Version", offered);
        }
        let (status, headers, body) = send(&edge.app, request.body(Body::empty()).unwrap()).await;
        assert_eq!(status, expected, "{offered:?}");
        assert_eq!(headers["awp-version"], "2.0", "{offered:?}");
        if status == StatusCode::NOT_ACCEPTABLE {
            assert_eq!(body["error"]["code"], "version_not_supported");
        }
    }
}

/// With `awp-v1`, the 1.x routes and the 2.0 routes serve side by side, each in its own
/// version.
#[cfg(feature = "awp-v1")]
#[tokio::test]
async fn v1_and_v2_serve_side_by_side() {
    let context = awp_types::BusinessContext::core("Shop", "A shop", "example.com");
    let v1 = adk_awp::awp_routes(
        adk_awp::AwpStateBuilder::new(Arc::new(arc_swap::ArcSwap::from_pointee(context))).build(),
    );
    let app = edge().app.merge(v1);
    let (status, headers, _) = send(
        &app,
        get("/awp/manifest").header("AWP-Version", "2.0, 1.0").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!((status, headers["awp-version"].to_str().unwrap()), (StatusCode::OK, "1.0"));
    let (status, headers, _) = send(
        &app,
        get("/awp/v2/manifest").header("AWP-Version", "2.0, 1.0").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!((status, headers["awp-version"].to_str().unwrap()), (StatusCode::OK, "2.0"));
    let (status, _, _) =
        send(&app, get("/awp/manifest").header("AWP-Version", "2.0").body(Body::empty()).unwrap())
            .await;
    assert_eq!(status, StatusCode::NOT_ACCEPTABLE, "a 2.0-only client gets 406 on 1.x");
}

fn tier() -> impl Strategy<Value = TrustTier> {
    proptest::sample::select(TrustTier::ALL.to_vec())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Tier filtering never shows a capability above the caller's tier, through the route
    /// (spec 3.5 §11).
    #[test]
    fn the_served_manifest_never_exceeds_the_tier(tiers in proptest::collection::vec(tier(), 0..8), caller in tier()) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut registry = CapabilityRegistry::new("example.com");
            for (index, min_trust) in tiers.iter().enumerate() {
                registry
                    .declare(
                        CapabilityV2::new(format!("cap_{index}"), "c", CapabilityEffect::ReadOnly).with_min_trust(*min_trust),
                        Arc::new(Counting::default()),
                    )
                    .unwrap();
            }
            let app = awp_v2_routes(EdgeState::builder(registry).tiers(Arc::new(HeaderTiers)).build());
            let (_, _, body) = send(
                &app,
                get("/awp/v2/manifest").header("x-test-tier", caller.as_str()).body(Body::empty()).unwrap(),
            )
            .await;
            let manifest: CapabilityManifestV2 = serde_json::from_value(body).unwrap();
            assert!(manifest.capabilities.iter().all(|c| c.min_trust <= caller));
            assert_eq!(manifest.capabilities.len(), tiers.iter().filter(|t| **t <= caller).count());
            // Calls follow the same rule: a hidden capability is not found.
            for (index, min_trust) in tiers.iter().enumerate() {
                let (status, _, _) = send(
                    &app,
                    call(&format!("cap_{index}")).header("x-test-tier", caller.as_str()).body(Body::from("{}")).unwrap(),
                )
                .await;
                let expected = if *min_trust <= caller { StatusCode::OK } else { StatusCode::NOT_FOUND };
                assert_eq!(status, expected);
            }
        });
    }
}
