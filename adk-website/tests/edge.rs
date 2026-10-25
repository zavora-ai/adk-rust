//! The packs' capability handlers on the agent edge, over HTTP (spec 3.5 §7.9, §7.10, §7.16
//! `packs`, A-I7, A-I8, A-I11, A-I25): an enquiry needs consent and becomes one lead, one
//! contact and one signal on the back office's agenda; a quote keeps its price into the
//! order and a return waits for the owner; payments wait for a rail; drift puts the edge in
//! read-only mode and only a person's close resumes it.

mod common;

use std::sync::Arc;

use adk_awp::v2::store::LeadStore;
use adk_core::principal::SubjectRef;
use adk_kernel::charter::{SignalFilter, SignalStore};
use adk_site::migrate::probe::{Drift, DriftKind, DriftLocation};
use adk_site::pipeline::{Change, EntryPoint, PipelineOutcome};
use adk_tool::SimpleToolContext;
use adk_website::alarms::AlarmRouter;
use adk_website::edge::handlers::ContactBook;
use adk_website::incident::{IncidentPath, StatusSink};
use adk_website::kinds::CharterInputs;
use adk_website::onboarding::build::SitePlan;
use adk_website::team::AgentRole;
use awp_types::{
    Channel, Charter, ContentId, ContentItem, ContentKind, ContentState, LeadRef, Markdown, Role,
    SignalKind, SiteKind, SiteState,
};
use axum::http::{StatusCode, header};
use common::edge::edge;
use common::*;
use serde_json::{Value, json};

/// Commits the kind's charter and publishes release 1: the plan's pages and `extra`.
async fn launch(site: &Site, name: &str, currency: &str, extra: Vec<ContentItem>) -> Charter {
    let mut inputs = CharterInputs::new(
        site.site.id,
        name,
        OWNER,
        "Fresh bread every morning.",
        vec!["en".parse().unwrap()],
        currency,
    );
    inputs.p1_channels = vec![Channel::Sms];
    let charter = site.template.draft_charter(&site.team, &inputs);
    site.commit(&charter, &[(Role::Owner, OWNER)]).await;
    let mut items = SitePlan::new(&charter, name).items(&charter, &[], &["KE".to_string()]);
    items.extend(extra);
    let change = Change {
        items,
        media: Vec::new(),
        entry: EntryPoint::SiteAgent { agent: AgentRole::Steward.principal(&site.site) },
        proposer: AgentRole::Steward.principal(&site.site),
        job: "onboarding".into(),
        action_class: "publish".into(),
    };
    let outcome = site.runtime.pipeline().run(change).await.unwrap();
    assert!(matches!(outcome, PipelineOutcome::Published(_)), "release 1: {outcome:?}");
    charter
}

fn product(sku: &str, title: &str, price: u64) -> ContentItem {
    ContentItem {
        id: ContentId(format!("products/{sku}/en")),
        kind: ContentKind::Product,
        slug: sku.into(),
        language: "en".parse().unwrap(),
        fields: [
            ("title".to_string(), json!(title)),
            ("summary".to_string(), json!(title)),
            ("sku".to_string(), json!(sku)),
            ("price".to_string(), json!(price)),
            ("currency".to_string(), json!("KES")),
            ("goal".to_string(), json!("sell")),
        ]
        .into_iter()
        .collect(),
        volatile: Vec::new(),
        body: Some(Markdown(format!("{title}, baked this morning.\n"))),
        media: Vec::new(),
        sources: Vec::new(),
        schedule: None,
        state: ContentState::Draft,
    }
}

fn names(manifest: &Value) -> Vec<String> {
    manifest["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn an_enquiry_needs_consent_and_becomes_one_lead_one_contact_and_one_signal() {
    let site = Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let edge = edge(&site, "juniper.example").await;
    let (status, _, manifest) = edge.send(edge.get("/awp/v2/manifest")).await;
    assert_eq!(status, StatusCode::OK);
    for capability in ["submit_inquiry", "check_availability", "search_site"] {
        assert!(names(&manifest).contains(&capability.to_string()), "{manifest}");
    }
    assert!(!names(&manifest).contains(&"get_quote".to_string()), "quotes are for known agents");

    let enquiry = json!({ "name": "Wanjiru", "email": "wanjiru@mail.example", "message": "Do you bake for weddings?" });
    let (status, _, refused) =
        edge.send(edge.call("submit_inquiry", "enq-1", None, &enquiry)).await;
    assert_eq!((status.as_u16(), &refused["error"]["code"]), (451, &json!("consent_required")));

    // A-I8: the visitor consents to be contacted; the receipt travels with the call.
    let receipt = edge.consent("contact").await;
    let call = || edge.call("submit_inquiry", "enq-1", Some(&receipt), &enquiry);
    let (status, _, received) = edge.send(call()).await;
    assert_eq!(status, StatusCode::OK, "{received}");
    assert_eq!(received["received"], true);
    let reference = received["reference"].as_str().unwrap().to_string();
    let (_, _, again) = edge.send(call()).await;
    assert_eq!(again["reference"], reference.as_str(), "A-I7: a retry is the same enquiry");

    let lead = edge.stores.leads.get(&LeadRef(reference.clone())).await.unwrap().unwrap();
    assert_eq!(lead.source, Channel::Http);
    assert_eq!(lead.consent.len(), 1);
    assert_eq!(lead.answers["message"].as_str(), "Do you bake for weddings?");
    assert!(
        !serde_json::to_string(&lead).unwrap().contains("wanjiru@"),
        "a lead carries no address"
    );
    let receipt_id: uuid::Uuid = receipt.parse().unwrap();
    let contacts = edge.contacts.by_receipts(&[receipt_id]).await.unwrap();
    assert_eq!(contacts.len(), 1);
    assert_eq!(contacts[0].email.as_deref(), Some("wanjiru@mail.example"));
    assert_eq!(contacts[0].contact_ref, lead.contact_ref);

    // One typed, screened signal on the back office's agenda (spec 3.5 §7.10).
    let records = site.signals.list(&SignalFilter::default()).await.unwrap();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].partition, site.partitions.back_office().id);
    match &records[0].signal.kind {
        SignalKind::LeadCaptured { lead_ref, answers, .. } => {
            assert_eq!(lead_ref.0, reference);
            assert_eq!(answers["capability"].as_str(), "submit_inquiry");
        }
        other => panic!("a lead_captured signal, not {other:?}"),
    }

    // The search pack answers from the release's index, read-only and public.
    let (status, _, results) =
        edge.send(edge.call("search_site", "s-1", None, &json!({ "query": "bread" }))).await;
    assert_eq!(status, StatusCode::OK, "{results}");
}

#[tokio::test]
async fn a_quote_keeps_its_price_into_the_order_and_a_return_waits_for_the_owner() {
    let site = Site::new(SiteKind::Ecommerce, "Juniper Bakery", "juniper.example", &["en"]).await;
    launch(&site, "Juniper Bakery", "KES", vec![product("rye-loaf", "Rye loaf", 450)]).await;
    let edge = edge(&site, "juniper.example").await;

    let (status, _, listed) = edge.send(edge.call("list_products", "l-1", None, &json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    assert_eq!(listed["release"], 1, "the catalogue is the signed release's (A-I26)");
    assert_eq!(listed["items"][0]["sku"], "rye-loaf");
    assert_eq!(listed["items"][0]["price"], 450);

    let quote_body = json!({ "items": [{ "sku": "rye-loaf", "quantity": 3 }] });
    let (status, _, _) = edge.send(edge.call("get_quote", "q-1", None, &quote_body)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "a public caller does not see quotes");
    let (status, _, quote) =
        edge.send(edge.signed(edge.call("get_quote", "q-1", None, &quote_body))).await;
    assert_eq!(status, StatusCode::OK, "{quote}");
    assert_eq!((quote["total"].as_u64(), quote["currency"].as_str()), (Some(1_350), Some("KES")));
    assert_eq!(quote["releaseSeq"], 1);

    let payment = edge.consent("payment_data").await;
    let order_body = json!({ "quoteId": quote["id"] });
    let (status, _, order) =
        edge.send(edge.signed(edge.call("place_order", "o-1", Some(&payment), &order_body))).await;
    assert_eq!(status, StatusCode::OK, "{order}");
    assert_eq!(order["status"], "awaiting_payment");
    assert_eq!(order["quote"]["total"], 1_350, "A-I25: the order keeps the quoted price");
    let order_id = order["id"].as_str().unwrap().to_string();
    let (_, _, tracked) = edge
        .send(edge.signed(edge.call("order_status", "st-1", None, &json!({ "orderId": order_id }))))
        .await;
    assert_eq!(tracked["status"], "awaiting_payment");

    let (status, _, refused) = edge
        .send(edge.signed(edge.call(
            "pay_order",
            "p-1",
            Some(&payment),
            &json!({ "orderId": order_id, "rail": "mpesa" }),
        )))
        .await;
    assert_eq!(refused["error"]["code"], "payment_policy_rejected", "{status} {refused}");

    // Every return is held for the owner (A-I7): 202, then completed once approved.
    let return_body = json!({ "orderId": order_id, "reason": "Arrived stale" });
    let (status, headers, held) =
        edge.send(edge.signed(edge.call("request_return", "r-1", None, &return_body))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{held}");
    assert_eq!(edge.edge.orders.get(&order_id).await.unwrap().status, "awaiting_payment");
    let location = headers[header::LOCATION].to_str().unwrap().to_string();
    assert_eq!(edge.approve_pending().await, 1, "the owner sees the return in the inbox");
    let (status, _, done) = edge.send(edge.signed(edge.get(&location))).await;
    assert_eq!((status, &done["state"]), (StatusCode::OK, &json!("completed")), "{done}");
    assert_eq!(done["result"]["status"], "return_approved");
}

#[tokio::test]
async fn drift_puts_the_edge_in_read_only_mode_and_only_a_persons_close_resumes_it() {
    let site = Site::new(SiteKind::Business, "Juniper Bakery", "juniper.example", &["en"]).await;
    let charter = launch(&site, "Juniper Bakery", "KES", Vec::new()).await;
    let edge = edge(&site, "juniper.example").await;
    let alarms = Arc::new(AlarmRouter::new(site.site.id, &charter.reporting));
    let status: Arc<dyn StatusSink> = Arc::new(edge.edge.status());
    let path = IncidentPath::new(site.runtime.clone(), site.ladder.clone(), alarms, status.clone());

    let drift = Drift {
        location: DriftLocation::Host,
        kind: DriftKind::Changed,
        path: "/index.html".into(),
        expected: Vec::new(),
        found: None,
        window: vec![1],
        detail: "the page matches no release in the window".into(),
    };
    let incident = path.open(vec![drift], 1, None).await.unwrap();
    let (code, _, document) = edge.send(edge.get("/awp/v2/status")).await;
    assert_eq!((code, &document["state"]), (StatusCode::OK, &json!("read_only")), "{document}");
    assert_eq!(document["reasonCode"], "drift_detected");

    // Writes answer 503 read_only with the signed status; reads and pages keep working.
    let receipt = edge.consent("contact").await;
    let enquiry = json!({ "name": "Wanjiru", "email": "w@mail.example", "message": "Open today?" });
    let (code, _, refused) =
        edge.send(edge.call("submit_inquiry", "ro-1", Some(&receipt), &enquiry)).await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE, "{refused}");
    assert_eq!(refused["status"]["reasonCode"], "drift_detected");
    let (code, _, _) =
        edge.send(edge.call("search_site", "ro-2", None, &json!({ "query": "bread" }))).await;
    assert_eq!(code, StatusCode::OK, "read-only capabilities answer");
    let (code, headers, _) = edge.send(edge.get("/")).await;
    assert_eq!(code, StatusCode::OK, "the release stays served");
    assert!(headers[header::CONTENT_TYPE].to_str().unwrap().starts_with("text/html"));

    // An agent cannot close it; the owner can, and the edge resumes.
    let agent = SubjectRef::service(AgentRole::Guardian.principal(&site.site));
    assert!(path.close(&incident.id, &agent, "dec-1").await.is_err());
    path.close(&incident.id, &SubjectRef::human(OWNER), "dec-1").await.unwrap();
    assert_eq!(status.get(site.site.id).await.unwrap().unwrap().state, SiteState::Operational);
    let (code, _, accepted) =
        edge.send(edge.call("submit_inquiry", "ro-3", Some(&receipt), &enquiry)).await;
    assert_eq!(code, StatusCode::OK, "{accepted}");

    // The Guardian's own reversible act: `edge_read_only` and back.
    let tool = edge.edge.read_only_tool();
    let ctx = Arc::new(SimpleToolContext::new("guardian"));
    let switched = tool.execute(ctx.clone(), json!({ "reason": "suspected_abuse" })).await.unwrap();
    assert_eq!(switched["readOnly"], true);
    assert_eq!(edge.send(edge.get("/awp/v2/status")).await.2["reasonCode"], "suspected_abuse");
    let resumed = tool.execute(ctx, json!({ "resume": true })).await.unwrap();
    assert_eq!(resumed["readOnly"], false);
}

#[tokio::test]
async fn every_release_is_announced_on_the_edges_events() {
    let site = Site::new(SiteKind::Ecommerce, "Juniper Bakery", "juniper.example", &["en"]).await;
    let edge = edge(&site, "juniper.example").await;
    launch(&site, "Juniper Bakery", "KES", vec![product("rye-loaf", "Rye loaf", 450)]).await;
    assert_eq!(*edge.announced.seen.lock().unwrap(), [(1, true)], "site.release.published");
}
