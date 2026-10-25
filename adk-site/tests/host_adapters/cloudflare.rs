//! Cloudflare: R2 (through the fake S3) under the conformance suite, purging a fake zone
//! through the Cloudflare API.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use adk_site::host::cloudflare::CloudflarePurge;
use adk_site::host::conformance::{self, Fixture};
use adk_site::host::object::{CachePurge, ObjectStoreHost};
use adk_site::host::s3::S3Store;
use adk_site::host::{HostTarget, SiteHost};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::support::{FakeServer, credential};

const TOKEN: &str = "cf-api-token";

#[derive(Default)]
struct FakeZone {
    purges: tokio::sync::Mutex<Vec<Value>>,
    refuse: AtomicBool,
}

async fn purge(
    State(zone): State<Arc<FakeZone>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {TOKEN}"))
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"success": false, "errors": [{"code": 10000}]})),
        )
            .into_response();
    }
    if id != "zone-1" || zone.refuse.load(Ordering::SeqCst) {
        return Json(json!({"success": false, "errors": [{"code": 1134, "message": "refused"}]}))
            .into_response();
    }
    zone.purges.lock().await.push(body);
    Json(json!({"success": true, "errors": [], "messages": [], "result": {"id": "zone-1"}}))
        .into_response()
}

async fn start() -> (FakeServer, Arc<FakeZone>) {
    let zone = Arc::new(FakeZone::default());
    let router = axum::Router::new()
        .route("/client/v4/zones/{id}/purge_cache", axum::routing::post(purge))
        .with_state(zone.clone());
    (FakeServer::start(router).await, zone)
}

fn purger(api: &FakeServer) -> CloudflarePurge {
    CloudflarePurge::new(
        "zone-1",
        "https://example.com/".parse().unwrap(),
        credential(&api.base, TOKEN),
    )
    .with_api(format!("{}/client/v4/", api.base).parse().unwrap())
}

fn r2(server: &FakeServer, prefix: &str) -> S3Store {
    S3Store::new(
        server.base.parse().unwrap(),
        "auto",
        "site",
        credential(&format!("{}/site", server.base), crate::s3::KEYS),
    )
    .path_style(true)
    .with_prefix(prefix)
    .with_name("r2")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r2_behind_a_worker_passes_the_conformance_suite_and_purges() {
    let (storage, fake) = crate::s3::start().await;
    let (api, zone) = start().await;
    let next = AtomicUsize::new(0);
    conformance::run_all(|| {
        let prefix = format!("site-{}", next.fetch_add(1, Ordering::SeqCst));
        Arc::new(
            ObjectStoreHost::prefix_pointer(Arc::new(r2(&storage, &prefix)))
                .with_name("cloudflare")
                .with_cache_purge(Arc::new(purger(&api))),
        ) as Arc<dyn SiteHost>
    })
    .await;
    assert_eq!(fake.refused.load(Ordering::SeqCst), 0);
    let purges = zone.purges.lock().await;
    assert!(!purges.is_empty());
    let urls: Vec<&str> = purges
        .iter()
        .flat_map(|p| p["files"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .collect();
    assert!(urls.contains(&"https://example.com/p0/"), "directory URLs are purged");
    assert!(urls.contains(&"https://example.com/.well-known/awp/log/checkpoint"));
    assert!(
        purges.iter().all(|p| p["files"].as_array().is_none_or(|f| f.len() <= 30)),
        "batches of 30"
    );
}

#[tokio::test]
async fn many_urls_purge_the_zone_and_a_refused_purge_does_not_fail_the_publish() {
    let (api, zone) = start().await;
    let many: Vec<String> = (0..400).map(|i| format!("/p{i}.html")).collect();
    purger(&api).purge(&many).await.unwrap();
    assert_eq!(zone.purges.lock().await[0], json!({"purge_everything": true}));

    zone.refuse.store(true, Ordering::SeqCst);
    assert!(purger(&api).purge(&["/a.html".into()]).await.is_err());
    let (storage, _) = crate::s3::start().await;
    let host = ObjectStoreHost::prefix_pointer(Arc::new(r2(&storage, "refused")))
        .with_cache_purge(Arc::new(purger(&api)));
    let mut fixture = Fixture::new().await;
    let release =
        fixture.release(&[("/index.html", b"<p>a</p>".to_vec())], Vec::new(), false).await;
    host.publish(&release, HostTarget::Production).await.expect("the release is live");
    assert_eq!(host.fetch("/").await.unwrap().as_deref(), Some(b"<p>a</p>".as_slice()));
}
