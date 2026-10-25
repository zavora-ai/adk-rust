//! A wake-up says nothing (spec 3.3 §5.4, §11 S-I5): for any tool, arguments and heading, the
//! body a phone decrypts is a version and a random id, and none of the decision's words.

#![cfg(feature = "mobile")]

mod mobile_support;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    ApprovalPolicy, ApprovalRequest, ApprovalRisk, ApprovalScope, ApprovalService,
    ApproverSelector, InMemoryApprovalStore, RequestContext, StoreApprovalService,
    ToolConfirmationRequest,
};
use adk_kernel::pairing::{DEVICE_ID_METADATA, InMemoryDeviceStore, IssueCode, Pairing};
use adk_session::InMemorySessionService;
use adk_surface::mobile::{Phone, PushAllowlist, PushDoorbell, PushKey};
use adk_surface::{LocalSource, SurfaceEngine};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use mobile_support::fake_push::{Browser, FakePush};
use proptest::prelude::*;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const OWNER: &str = "ana";

#[test]
fn a_wake_up_carries_none_of_the_decision() {
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let (fake, base, approvals, browser, task, server) = runtime.block_on(async {
        let (fake, base, server) = FakePush::start().await;
        let doorbell = Arc::new(PushDoorbell::new());
        let approvals: Arc<dyn ApprovalService> = Arc::new(
            StoreApprovalService::new(Arc::new(InMemoryApprovalStore::new()))
                .with_notifier(doorbell.clone()),
        );
        let source = LocalSource::builder("home")
            .sessions(Arc::new(InMemorySessionService::new()))
            .approvals(Arc::clone(&approvals))
            .build()
            .unwrap();
        let engine = SurfaceEngine::builder(Arc::new(source)).build();
        let pairing = Pairing::new(Arc::new(InMemoryDeviceStore::new()));
        let issued = pairing.issue(IssueCode::new(OWNER, ["owner"])).await.unwrap();
        let device = pairing.claim(&issued.code, None).await.unwrap().device.id;
        let phone = Phone::builder(engine, pairing)
            .home_url("http://localhost:4100")
            .push_key(PushKey::generate().unwrap())
            .doorbell(doorbell)
            .approvals(Arc::clone(&approvals))
            .push_allowlist(PushAllowlist::new(["localhost"]).with_plain_http_to_localhost())
            .build()
            .unwrap();
        let browser = Browser::new();
        let context = RequestContext {
            user_id: OWNER.into(),
            scopes: vec!["owner".into()],
            metadata: HashMap::from([
                (DEVICE_ID_METADATA.to_string(), device),
                ("adk.device.kind".to_string(), "phone".to_string()),
            ]),
        };
        let app = axum::Router::new()
            .nest("/api", adk_surface::mobile::router(phone.clone()))
            .layer(axum::Extension(Some(context)));
        let request = Request::put("/api/surface/phone/push")
            .header("content-type", "application/json")
            .body(Body::from(browser.subscription(&format!("{base}/push/1")).to_string()))
            .unwrap();
        assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::NO_CONTENT);
        let task = phone.start_push().unwrap();
        (fake, base, approvals, browser, task, server)
    });
    let _ = base;

    let word = "[A-Za-z][A-Za-z0-9_ ]{5,30}";
    let config = ProptestConfig { cases: 100, ..ProptestConfig::default() };
    let cases = std::sync::atomic::AtomicUsize::new(0);
    proptest!(config, |(
        tool in "[a-z][a-z_]{5,30}",
        args in proptest::collection::btree_map(word, word, 1..4),
        heading in word,
    )| {
        let case = cases.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let body = runtime.block_on(async {
            let call = ToolConfirmationRequest::new(tool.clone(), json!(args));
            let request = ApprovalRequest::new(
                format!("apr-{case}"),
                ApprovalScope::new("home", OWNER, "s1"),
                &call,
                ApprovalPolicy::new([ApproverSelector::group("owner")]).allow_self_approval(),
            )
            .with_summary(heading.clone())
            .with_risk(ApprovalRisk::Medium);
            let before = fake.count();
            approvals.submit(request).await.unwrap();
            fake.wait_for(before + 1).await;
            fake.got.lock().unwrap()[before].1.clone()
        });
        let payload = browser.decrypt(&body);
        let fields: BTreeMap<String, Value> =
            serde_json::from_value(payload.clone()).unwrap();
        prop_assert_eq!(fields.keys().cloned().collect::<Vec<_>>(), vec!["id".to_string(), "v".to_string()]);
        prop_assert_eq!(&fields["v"], &json!(1));
        let id = fields["id"].as_str().unwrap();
        prop_assert!(id.len() == 22 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'), "{}", id);
        let plain = payload.to_string();
        for word in std::iter::once(&tool).chain(args.keys()).chain(args.values()).chain(std::iter::once(&heading)) {
            prop_assert!(!plain.contains(word.as_str()), "{} leaked into {}", word, plain);
        }
    });
    task.abort();
    server.abort();
    runtime.block_on(async { tokio::time::sleep(Duration::from_millis(10)).await });
}
