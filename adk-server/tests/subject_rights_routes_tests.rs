//! The subject rights routes act only for a signed-in caller, and a link needs the visitor's
//! token and a fresh sign-in, once.

#![cfg(feature = "awp")]

use std::sync::Arc;

use adk_auth::VisitorTokens;
use adk_core::RequestContext;
use adk_core::principal::SubjectRef;
use adk_core::subject::{
    InMemoryConsentStore, InMemorySubjectRegistry, SubjectRegistry, SubjectRights, VisitorId,
};
use adk_server::subject_rights::{
    AUTH_TIME_METADATA, SubjectRightsEndpoints, subject_rights_router,
};
use axum::Extension;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::Value;
use tower::ServiceExt;

const KEY: &[u8] = b"the site's visitor token key, 32 bytes";

fn context(signed_in_secs_ago: Option<i64>) -> RequestContext {
    let mut metadata = std::collections::HashMap::new();
    if let Some(ago) = signed_in_secs_ago {
        metadata.insert(
            AUTH_TIME_METADATA.to_string(),
            (chrono::Utc::now().timestamp() - ago).to_string(),
        );
    }
    RequestContext { user_id: "alice".into(), scopes: vec![], metadata }
}

async fn send(
    rights: &SubjectRights,
    caller: Option<RequestContext>,
    request: Request<Body>,
) -> (StatusCode, Option<String>, Value) {
    let router = subject_rights_router(SubjectRightsEndpoints::new(
        rights.clone(),
        VisitorTokens::new(KEY.to_vec()),
    ))
    .layer(Extension(caller));
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().to_owned());
    let body = axum::body::to_bytes(response.into_body(), 1 << 20).await.unwrap();
    (status, cookie, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn link_request(token: &str) -> Request<Body> {
    Request::post("/subject/link").header("awp-subject", token).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn links_need_a_token_and_a_fresh_sign_in_once() {
    let registry = Arc::new(InMemorySubjectRegistry::new());
    let rights = SubjectRights::new(registry.clone(), Arc::new(InMemoryConsentStore::new()))
        .with_app("shop");
    let visitor = VisitorId::generate();
    let token = VisitorTokens::new(KEY.to_vec()).issue(&visitor);

    let anonymous =
        send(&rights, None, Request::get("/subject/export").body(Body::empty()).unwrap()).await;
    assert_eq!(anonymous.0, StatusCode::UNAUTHORIZED);

    let stale = send(&rights, Some(context(Some(3_600))), link_request(&token)).await;
    assert_eq!(stale.0, StatusCode::UNAUTHORIZED, "{}", stale.2);
    let unsigned = send(&rights, Some(context(None)), link_request(&token)).await;
    assert_eq!(unsigned.0, StatusCode::UNAUTHORIZED);
    let forged = send(&rights, Some(context(Some(5))), link_request("v1.v:00.1.00")).await;
    assert_eq!(forged.0, StatusCode::BAD_REQUEST);

    let (status, cookie, _) = send(&rights, Some(context(Some(5))), link_request(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(cookie.unwrap().contains("Max-Age=0"), "the visitor cookie is cleared");
    assert_eq!(
        registry.linked(&SubjectRef::human("alice")).await.unwrap(),
        vec![visitor.subject()]
    );
    let again = send(&rights, Some(context(Some(5))), link_request(&token)).await;
    assert_eq!(again.0, StatusCode::CONFLICT, "a visitor id links once");

    let (status, _, exported) = send(
        &rights,
        Some(context(None)),
        Request::get("/subject/export").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(exported["linked"][0]["id"], visitor.as_str());

    let (status, _, report) = send(
        &rights,
        Some(context(None)),
        Request::post("/subject/erase").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(report["status"], "completed");
    assert_eq!(report["subjects"], 2, "erasure covers the linked visitor id");
}
