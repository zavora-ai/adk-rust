//! S15 — agency mode (spec 3.5 §5 S15, §7.22, A-I22).
//!
//! One harness runs two sites for two owners: a bakery and a clinic. Each has its own
//! partitions, memory, wallet, drivers and edge.
//!
//! Pass condition: each site's partitions, memory, wallet and drivers are isolated; a
//! cross-site request is denied and recorded even for a coordinator.

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::principal::SubjectRef;
use adk_core::{
    ActingContext, AgentPrincipal, ManualClock, PolicyCall, PolicyDecision, ToolOrigin, ToolPolicy,
    ToolPolicyRequest,
};
use adk_kernel::Partitions;
use adk_kernel::audit::PARTITION_CROSSING;
use adk_kernel::charter::KpiSource;
use adk_payments::wallet::{
    InMemoryWalletProvider, InMemoryWalletStore, Wallet, WalletKpiSource, WalletPolicy,
};
use adk_website::deploy::agency::{CROSS_SITE_TOOL, REQUEST_REFUSED};
use adk_website::deploy::{Agency, AgencyRouter};
use adk_website::wallet::WalletKpi;
use anyhow::Result;
use awp_types::{LeadRef, SiteKind};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::checks::Checks;
use crate::fixture::edge::{EdgeSite, edge};
use crate::fixture::*;
use crate::live::Mode;

fn crossing(target: &str, agent: &str, what: &str) -> ToolPolicyRequest {
    let mut request =
        ToolPolicyRequest::new(CROSS_SITE_TOOL, json!({ "partition": target, "query": what }));
    request.agent_name = agent.into();
    request.invocation_id = format!("inv-{agent}");
    request.acting = Some(ActingContext::new(AgentPrincipal::derived("agency", agent)));
    request
}

async fn send(
    router: &axum::Router,
    request: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let response = router.clone().oneshot(request).await.expect("the router answers");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap_or_default();
    (status, headers, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn at(host: &str, mut request: Request<Body>) -> Request<Body> {
    request.headers_mut().insert(header::HOST, host.parse().expect("a host"));
    request
}

fn wallet(name: &str) -> Result<Wallet> {
    let clock: Arc<dyn adk_core::Clock> = Arc::new(ManualClock::new(chrono::Utc::now()));
    Ok(Wallet::builder(
        name,
        WalletPolicy::new("KES"),
        Arc::new(
            InMemoryWalletProvider::stored_value("treasury", "KES", 10_000)
                .with_clock(clock.clone()),
        ),
    )
    .store(Arc::new(InMemoryWalletStore::new()))
    .owner(SubjectRef::human(OWNER))
    .clock(clock)
    .build()?)
}

/// Runs S15.
pub async fn run(_mode: Mode) -> Result<Checks> {
    let mut checks = Checks::default();
    let bakery =
        Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await?;
    let clinic = Site::new(SiteKind::Health, "Mvule Clinic", "mvule.example", &["en"]).await?;
    bakery.launch("Juniper Bakery", "KES", Vec::new()).await?;
    let recorder = InMemoryAuditRecorder::new();
    let table = Partitions::in_memory().with_audit(AuditTrail::new(Arc::new(recorder.clone())));
    let mut agency = Agency::new(table);
    agency.add(bakery.site.clone()).await?;
    agency.add(clinic.site.clone()).await?;
    checks.check(
        "one harness runs both sites, each its own",
        agency.sites().count() == 2 && agency.add(bakery.site.clone()).await.is_err(),
        "a site cannot be added twice",
    );

    // Partitions, memory and drivers: a request from the bakery's back office into the
    // clinic's — by the bakery's Steward and by the agency's own coordinator — is a crossing,
    // denied and recorded.
    let clinic_memory = agency.partitions(&clinic.site.name).map(|p| p.back_office().id.clone());
    let bakery_memory = agency.partitions(&bakery.site.name).map(|p| p.back_office().id.clone());
    let (Some(clinic_memory), Some(bakery_memory)) = (clinic_memory, bakery_memory) else {
        anyhow::bail!("both sites have partitions");
    };
    let policy = agency
        .back_office_policy(&bakery.site.name, "agency-coordinator")
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let origin =
        ToolOrigin::new(adk_core::STATIC_TOOL_ORIGIN, &json!({ "name": CROSS_SITE_TOOL }), None);
    let mut denied = Vec::new();
    for (agent, what) in [
        ("steward", "their patients' notes"),
        ("agency-coordinator", "their email driver"),
        ("finance", "their wallet"),
    ] {
        let decided = policy
            .evaluate_call(
                &crossing(clinic_memory.as_str(), agent, what),
                &PolicyCall::new().with_origin(&origin).with_function_call_id(agent),
            )
            .await;
        denied.push((agent, matches!(decided, PolicyDecision::Deny { .. })));
    }
    let own = policy
        .evaluate_call(
            &crossing(bakery_memory.as_str(), "steward", "our notes"),
            &PolicyCall::new().with_origin(&origin).with_function_call_id("own"),
        )
        .await;
    let crossings: Vec<_> =
        recorder.records().into_iter().filter(|r| r.action == PARTITION_CROSSING).collect();
    checks.check(
        "a cross-site request is denied and recorded, even for the coordinator",
        denied.iter().all(|(_, d)| *d)
            && own == PolicyDecision::Allow
            && crossings.len() >= 3
            && format!("{:?}", crossings.iter().map(|r| &r.ext).collect::<Vec<_>>())
                .contains(clinic_memory.as_str()),
        format!("{denied:?}; its own memory is allowed; {} crossings recorded", crossings.len()),
    );

    // Memory: each site's facts live in its own partition's scope.
    let bakery_scope = bakery.partitions.fact_scope(OWNER);
    let clinic_scope = clinic.partitions.fact_scope(OWNER);
    checks.check(
        "each site's memory scope is its own",
        format!("{bakery_scope:?}") != format!("{clinic_scope:?}")
            && format!("{bakery_scope:?}").contains(&bakery.site.name)
            && !format!("{bakery_scope:?}").contains(&clinic.site.name),
        format!("{bakery_scope:?} / {clinic_scope:?}"),
    );

    // Wallets: each answers for its own site only.
    let bakery_wallet =
        WalletKpi::new(WalletKpiSource::new(wallet(&bakery.site.name)?, bakery.site.id));
    let now = chrono::Utc::now();
    let own =
        bakery_wallet.read(bakery.site.id, "spend", now - chrono::Duration::days(7), now).await;
    let other =
        bakery_wallet.read(clinic.site.id, "spend", now - chrono::Duration::days(7), now).await;
    checks.check(
        "each site's wallet answers for its own site only",
        own.is_ok() && other.is_err(),
        format!("own {own:?}; the other site: refused"),
    );

    // Edges: each request reaches only the site its host names.
    let shop = Site::new(SiteKind::Ecommerce, "Juniper Shop", "shop.example", &["en"]).await?;
    agency.add(shop.site.clone()).await?;
    let (bakery_edge, clinic_edge) =
        (edge(&bakery, "juniper.example").await?, edge(&clinic, "mvule.example").await?);
    let shop_edge = edge(&shop, "shop.example").await?;
    let edge_audit = InMemoryAuditRecorder::new();
    let router = AgencyRouter::new(BTreeMap::from([
        ("juniper.example".to_string(), bakery_edge.router.clone()),
        ("mvule.example".to_string(), clinic_edge.router.clone()),
        ("shop.example".to_string(), shop_edge.router.clone()),
    ]))
    .with_audit(AuditTrail::new(Arc::new(edge_audit.clone())))
    .router();
    let (_, _, bakery_manifest) =
        send(&router, at("juniper.example", bakery_edge.get("/awp/v2/manifest"))).await;
    let (_, _, clinic_manifest) =
        send(&router, at("mvule.example:443", clinic_edge.get("/awp/v2/manifest"))).await;
    let (b, c) = (EdgeSite::names(&bakery_manifest), EdgeSite::names(&clinic_manifest));
    checks.check(
        "each host reaches its own site's capabilities",
        b.contains(&"submit_inquiry".to_string())
            && c.contains(&"practice_information".to_string())
            && !b.contains(&"practice_information".to_string()),
        format!("bakery {}, clinic {} capabilities", b.len(), c.len()),
    );
    let grant = json!({ "scope": "contact", "policyVersion": "1", "jurisdiction": "KE" });
    let (_, _, receipt) =
        send(&router, at("juniper.example", bakery_edge.post("/awp/v2/consent", &grant))).await;
    let receipt = receipt["id"].as_str().unwrap_or_default().to_string();
    let enquiry =
        json!({ "name": "Wanjiru", "email": "w@mail.example", "message": "Gluten free?" });
    let (status, _, received) = send(
        &router,
        at("juniper.example", bakery_edge.call("submit_inquiry", "a-1", Some(&receipt), &enquiry)),
    )
    .await;
    let lead = LeadRef(received["reference"].as_str().unwrap_or_default().to_string());
    use adk_awp::v2::store::LeadStore;
    checks.check(
        "an enquiry to the bakery is the bakery's lead only",
        status == StatusCode::OK
            && bakery_edge.stores.leads.get(&lead).await?.is_some()
            && clinic_edge.stores.leads.get(&lead).await?.is_none(),
        format!("{status}"),
    );

    // A held call of one site — a return the shop holds for its owner — is invisible at
    // another.
    let request = json!({ "orderId": "o-unknown", "reason": "stale" });
    let (held, headers, _) = send(
        &router,
        at(
            "shop.example",
            shop_edge.signed(shop_edge.call("request_return", "r-1", None, &request)),
        ),
    )
    .await;
    let location =
        headers.get(header::LOCATION).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let (there, _, _) =
        send(&router, at("mvule.example", clinic_edge.signed(clinic_edge.get(&location)))).await;
    let (here, _, _) =
        send(&router, at("shop.example", shop_edge.signed(shop_edge.get(&location)))).await;
    checks.check(
        "a held call of one site is invisible at the other",
        held == StatusCode::ACCEPTED && there == StatusCode::NOT_FOUND && here == StatusCode::OK,
        format!("held {held} at the shop; at the clinic {there}; at the shop {here}"),
    );
    let (unknown, _, body) =
        send(&router, at("other.example", bakery_edge.get("/awp/v2/manifest"))).await;
    let refusals = edge_audit.records().into_iter().filter(|r| r.action == REQUEST_REFUSED).count();
    checks.check(
        "an unknown host is refused and recorded",
        unknown == StatusCode::NOT_FOUND
            && body["error"]["code"] == "resource_not_found"
            && refusals == 1,
        format!("{unknown}, {refusals} refusal recorded"),
    );
    Ok(checks)
}
