//! Agency mode (spec 3.5 §7.22, A-I22): one harness runs two clients' sites. A request from
//! one site's back office into the other's memory — the agency's coordinator's included — is
//! denied and recorded as a crossing; at the edge each request reaches only the site its host
//! names, a held call of one site is invisible at the other, and an unknown host is refused
//! and recorded.

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use adk_awp::v2::store::LeadStore;
use adk_core::audit_log::{AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ActingContext, AgentPrincipal, PolicyCall, PolicyDecision, ToolOrigin, ToolPolicy,
    ToolPolicyRequest,
};
use adk_kernel::Partitions;
use adk_kernel::audit::PARTITION_CROSSING;
use adk_website::deploy::agency::{CROSS_SITE_TOOL, REQUEST_REFUSED};
use adk_website::deploy::{Agency, AgencyRouter};
use awp_types::{LeadRef, SiteKind};
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::edge::edge;
use common::*;
use serde_json::{Value, json};
use tower::ServiceExt;

fn crossing_request(target: &str, agent: &str) -> ToolPolicyRequest {
    let mut request = ToolPolicyRequest::new(
        CROSS_SITE_TOOL,
        json!({ "partition": target, "query": "their customers" }),
    );
    request.agent_name = agent.into();
    request.invocation_id = format!("inv-{agent}");
    request.acting = Some(ActingContext::new(AgentPrincipal::derived("agency", agent)));
    request
}

async fn send(router: &axum::Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn at(host: &str, mut request: Request<Body>) -> Request<Body> {
    request.headers_mut().insert(header::HOST, host.parse().unwrap());
    request
}

#[tokio::test]
async fn a_site_never_reaches_another_sites_partitions() {
    let bakery = Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let clinic = Site::new(SiteKind::Health, "Mvule Clinic", "mvule.example", &["en"]).await;
    let recorder = InMemoryAuditRecorder::new();
    let table = Partitions::in_memory().with_audit(AuditTrail::new(Arc::new(recorder.clone())));
    let mut agency = Agency::new(table);
    agency.add(bakery.site.clone()).await.unwrap();
    agency.add(clinic.site.clone()).await.unwrap();
    assert!(agency.add(bakery.site.clone()).await.is_err(), "every site is its own");
    assert_eq!(agency.sites().count(), 2);
    let clinic_memory = agency.partitions(&clinic.site.name).unwrap().back_office().id.clone();
    let bakery_memory = agency.partitions(&bakery.site.name).unwrap().back_office().id.clone();

    // The bakery's Steward and the agency's own coordinator both ask for the clinic's memory.
    let policy = agency.back_office_policy(&bakery.site.name, "agency-coordinator").unwrap();
    let origin =
        ToolOrigin::new(adk_core::STATIC_TOOL_ORIGIN, &json!({ "name": CROSS_SITE_TOOL }), None);
    for (agent, call) in [("steward", "c1"), ("agency-coordinator", "c2")] {
        let request = crossing_request(clinic_memory.as_str(), agent);
        let decided = policy
            .evaluate_call(
                &request,
                &PolicyCall::new().with_origin(&origin).with_function_call_id(call),
            )
            .await;
        assert!(matches!(decided, PolicyDecision::Deny { .. }), "{agent}: {decided:?}");
    }
    // Its own memory is not a crossing.
    let own = crossing_request(bakery_memory.as_str(), "steward");
    let decided = policy
        .evaluate_call(&own, &PolicyCall::new().with_origin(&origin).with_function_call_id("c3"))
        .await;
    assert_eq!(decided, PolicyDecision::Allow);

    let crossings: Vec<_> =
        recorder.records().into_iter().filter(|r| r.action == PARTITION_CROSSING).collect();
    assert!(!crossings.is_empty(), "the denied crossings are recorded");
    let text = format!("{:?}", crossings.iter().map(|r| &r.ext).collect::<Vec<_>>());
    assert!(text.contains(clinic_memory.as_str()), "{text}");
}

#[tokio::test]
async fn each_request_reaches_only_the_site_its_host_names() {
    let bakery = Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let clinic = Site::new(SiteKind::Health, "Mvule Clinic", "mvule.example", &["en"]).await;
    let (bakery_edge, clinic_edge) =
        (edge(&bakery, "juniper.example").await, edge(&clinic, "mvule.example").await);
    let recorder = InMemoryAuditRecorder::new();
    let router = AgencyRouter::new(BTreeMap::from([
        ("juniper.example".to_string(), bakery_edge.router.clone()),
        ("mvule.example".to_string(), clinic_edge.router.clone()),
    ]))
    .with_audit(AuditTrail::new(Arc::new(recorder.clone())))
    .router();

    let names = |manifest: &Value| -> Vec<String> {
        manifest["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap().to_string())
            .collect()
    };
    let (_, bakery_manifest) =
        send(&router, at("juniper.example", bakery_edge.get("/awp/v2/manifest"))).await;
    let (_, clinic_manifest) =
        send(&router, at("mvule.example:443", clinic_edge.get("/awp/v2/manifest"))).await;
    assert!(names(&bakery_manifest).contains(&"submit_inquiry".to_string()));
    assert!(names(&clinic_manifest).contains(&"practice_information".to_string()));
    assert!(!names(&bakery_manifest).contains(&"practice_information".to_string()));

    // An enquiry to the bakery is the bakery's lead only.
    let receipt = {
        let grant = json!({ "scope": "contact", "policyVersion": "1", "jurisdiction": "KE" });
        let (status, receipt) =
            send(&router, at("juniper.example", bakery_edge.post("/awp/v2/consent", &grant))).await;
        assert_eq!(status, StatusCode::CREATED, "{receipt}");
        receipt["id"].as_str().unwrap().to_string()
    };
    let enquiry =
        json!({ "name": "Wanjiru", "email": "w@mail.example", "message": "Gluten free?" });
    let (status, received) = send(
        &router,
        at("juniper.example", bakery_edge.call("submit_inquiry", "a-1", Some(&receipt), &enquiry)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{received}");
    let lead = LeadRef(received["reference"].as_str().unwrap().to_string());
    assert!(bakery_edge.stores.leads.get(&lead).await.unwrap().is_some());
    assert!(clinic_edge.stores.leads.get(&lead).await.unwrap().is_none());
    // The same call at the clinic's host reaches the clinic, which declares no such form.
    let (status, _) = send(
        &router,
        at("mvule.example", clinic_edge.call("submit_inquiry", "a-2", Some(&receipt), &enquiry)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // An unknown host is refused, recorded, and reaches no site.
    let (status, body) =
        send(&router, at("other.example", bakery_edge.get("/awp/v2/manifest"))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "resource_not_found");
    let refusals: Vec<_> =
        recorder.records().into_iter().filter(|r| r.action == REQUEST_REFUSED).collect();
    assert_eq!(refusals.len(), 1);
}
