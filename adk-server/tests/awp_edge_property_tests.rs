//! Properties of the AWP 2.0 edge (spec 3.5 §11): no binding ever shows a capability above
//! the caller's tier, and a `non_idempotent` capability executes at most once per key (A-I7).

#![cfg(feature = "awp")]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_awp::signing::jws::{DocumentSigner, Ed25519DocumentSigner};
use adk_awp::v2::capability::{
    CapabilityCall, CapabilityError, CapabilityHandler, CapabilityRegistry,
};
use adk_core::Url;
use adk_server::awp::tiers::ApiKeyCredentials;
use adk_server::awp::{AwpEdge, EdgeKeys, EdgeLimits, EdgeTiers};
use adk_tool::mcp::server::McpSessions;
use async_trait::async_trait;
use awp_types::{CapabilityEffect, CapabilityV2, RateClass, TrustRule, TrustTier};
use axum::Router;
use axum::body::Body;
use axum::http::{Request, header};
use proptest::prelude::*;
use serde_json::{Value, json};
use tower::ServiceExt;

const EDGE: &str = "https://agent.example.com";

struct Counted(Arc<AtomicUsize>);

#[async_trait]
impl CapabilityHandler for Counted {
    async fn handle(&self, call: CapabilityCall) -> Result<Value, CapabilityError> {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        Ok(json!({ "run": n, "amount": call.input["amount"] }))
    }
}

fn keys() -> EdgeKeys {
    EdgeKeys {
        card: Arc::new(Ed25519DocumentSigner::from_seed("card", [5; 32]).unwrap())
            as Arc<dyn DocumentSigner>,
        receipt: Arc::new(Ed25519DocumentSigner::from_seed("receipt", [6; 32]).unwrap()),
    }
}

/// API keys that earn each tier above `public`.
fn tiers() -> EdgeTiers {
    let mut credentials = ApiKeyCredentials::new();
    let mut builder = EdgeTiers::builder();
    for tier in [TrustTier::Known, TrustTier::Verified, TrustTier::Partner] {
        credentials = credentials.key_secret(tier.as_str(), "secret");
        builder = builder.private_rule(TrustRule {
            subject: format!("key:{tier}"),
            tier,
            rate_class: None,
            note: None,
        });
    }
    let generous = RateClass { requests_per_minute: 100_000, calls_per_day: 100_000 };
    builder.verifier(credentials).rate_class("generous", generous).build()
}

fn limits() -> EdgeLimits {
    let generous = RateClass { requests_per_minute: 100_000, calls_per_day: 100_000 };
    TrustTier::ALL
        .into_iter()
        .fold(EdgeLimits::default(), |limits, tier| limits.tier_default(tier, generous))
}

fn request(tier: TrustTier, request: axum::http::request::Builder) -> axum::http::request::Builder {
    match tier {
        TrustTier::Public => request,
        tier => request.header(header::AUTHORIZATION, format!("ApiKey {tier}:secret")),
    }
}

async fn json_of(router: &Router, request: Request<Body>) -> Value {
    let response = router.clone().oneshot(request).await.unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn tier_strategy() -> impl Strategy<Value = TrustTier> {
    prop::sample::select(TrustTier::ALL.to_vec())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// The manifest, the Agent Card and MCP `tools/list` list exactly the capabilities whose
    /// minimum tier is at or below the caller's (spec 3.5 §11; AWP 2.0 §7.3).
    #[test]
    fn no_binding_shows_a_capability_above_the_tier(
        minimums in prop::collection::vec(tier_strategy(), 1..8),
        caller in tier_strategy(),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut registry = CapabilityRegistry::new("example.com");
            let mut expected = BTreeSet::new();
            for (index, minimum) in minimums.iter().enumerate() {
                let name = format!("cap_{index}");
                if *minimum <= caller {
                    expected.insert(name.clone());
                }
                registry
                    .declare(
                        CapabilityV2::new(name, "A capability", CapabilityEffect::ReadOnly).with_min_trust(*minimum),
                        Arc::new(Counted(Arc::new(AtomicUsize::new(0)))),
                    )
                    .unwrap();
            }
            let edge = AwpEdge::builder(registry, keys(), Url::parse(EDGE).unwrap())
                .tiers(tiers())
                .limits(limits())
                .mcp_sessions(McpSessions::Stateless)
                .build()
                .unwrap();
            let router = edge.router();
            let names = |items: &Value, key: &str| -> BTreeSet<String> {
                items.as_array().unwrap().iter().map(|item| item[key].as_str().unwrap().to_owned()).collect()
            };
            let manifest = json_of(&router, request(caller, Request::get(format!("{EDGE}/awp/v2/manifest"))).body(Body::empty()).unwrap()).await;
            prop_assert_eq!(&manifest["tier"], &json!(caller.as_str()));
            prop_assert_eq!(names(&manifest["capabilities"], "name"), expected.clone());
            let card = json_of(&router, request(caller, Request::get(format!("{EDGE}/.well-known/agent-card.json"))).body(Body::empty()).unwrap()).await;
            prop_assert_eq!(names(&card["skills"], "id"), expected.clone());
            let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
            let tools = json_of(
                &router,
                request(caller, Request::post(format!("{EDGE}/mcp")))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT, "application/json, text/event-stream")
                    .body(Body::from(list.to_string()))
                    .unwrap(),
            )
            .await;
            prop_assert_eq!(names(&tools["result"]["tools"], "name"), expected);
            Ok(())
        })?;
    }

    /// A `non_idempotent` capability runs once per key; a repeat returns the first outcome
    /// and the same key with another input is a conflict (A-I7; AWP 2.0 §7.7).
    #[test]
    fn a_key_executes_at_most_once(calls in prop::collection::vec((0usize..4, prop::bool::ANY), 1..24)) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let runs = Arc::new(AtomicUsize::new(0));
            let mut registry = CapabilityRegistry::new("example.com");
            registry
                .declare(
                    CapabilityV2::new("book", "Book", CapabilityEffect::NonIdempotent).with_min_trust(TrustTier::Known),
                    Arc::new(Counted(Arc::clone(&runs))),
                )
                .unwrap();
            let edge = AwpEdge::builder(registry, keys(), Url::parse(EDGE).unwrap())
                .tiers(tiers())
                .limits(limits())
                .build()
                .unwrap();
            let router = edge.router();
            let mut first: BTreeMap<usize, (u64, Value)> = BTreeMap::new();
            for (key, large) in calls {
                let amount = if large { 2_000 } else { 1_000 };
                let call = request(TrustTier::Known, Request::post(format!("{EDGE}/awp/v2/capabilities/book")))
                    .header(header::CONTENT_TYPE, "application/json")
                    .header("idempotency-key", format!("key-{key}"))
                    .body(Body::from(json!({ "amount": amount }).to_string()))
                    .unwrap();
                let answer = json_of(&router, call).await;
                match first.get(&key) {
                    None => {
                        prop_assert_eq!(&answer["amount"], &json!(amount));
                        first.insert(key, (amount, answer));
                    }
                    Some((recorded, output)) if *recorded == amount => prop_assert_eq!(&answer, output),
                    Some(_) => prop_assert_eq!(&answer["error"]["code"], &json!("idempotency_conflict")),
                }
            }
            prop_assert_eq!(runs.load(Ordering::SeqCst), first.len());
            Ok(())
        })?;
    }
}
