//! `GcsStore` against a fake of the Cloud Storage JSON API (multipart upload, media
//! download, multi-step rewrite, delete), and against a real bucket when one is configured.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_site::host::SiteHost;
use adk_site::host::conformance;
use adk_site::host::gcs::GcsStore;
use adk_site::host::object::ObjectStoreHost;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::support::{FakeServer, credential, decode};

const TOKEN: &str = "ya29.fake-access-token";

#[derive(Default)]
struct FakeGcs {
    objects: tokio::sync::Mutex<BTreeMap<String, (Bytes, Value)>>,
    rewrites: AtomicUsize,
    unauthorised: AtomicUsize,
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({"error": {"code": 404, "message": "No such object"}})))
        .into_response()
}

/// Splits a `multipart/related` body into its metadata and media.
fn multipart(content_type: &str, body: &[u8]) -> (Value, Bytes) {
    let boundary = content_type.split("boundary=").nth(1).expect("boundary");
    let text = body;
    let delimiter = format!("--{boundary}");
    let find = |from: usize, needle: &[u8]| {
        text[from..].windows(needle.len()).position(|w| w == needle).map(|p| p + from)
    };
    let first = find(0, delimiter.as_bytes()).expect("first part");
    let meta_start = find(first, b"\r\n\r\n").expect("metadata headers") + 4;
    let meta_end = find(meta_start, format!("\r\n{delimiter}").as_bytes()).expect("metadata end");
    let media_start = find(meta_end + 2, b"\r\n\r\n").expect("media headers") + 4;
    let media_end =
        find(media_start, format!("\r\n{delimiter}--").as_bytes()).expect("closing delimiter");
    (
        serde_json::from_slice(&text[meta_start..meta_end]).expect("metadata JSON"),
        Bytes::copy_from_slice(&text[media_start..media_end]),
    )
}

async fn handle(State(fake): State<Arc<FakeGcs>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    if parts.headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {TOKEN}"))
    {
        fake.unauthorised.fetch_add(1, Ordering::SeqCst);
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": {"code": 401}}))).into_response();
    }
    let segments: Vec<String> = parts.uri.path().split('/').map(decode).collect();
    let query = parts.uri.query().unwrap_or_default();
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let mut objects = fake.objects.lock().await;
    match (parts.method.clone(), segments.as_slice()) {
        (Method::POST, ["", "upload", "storage", "v1", "b", bucket, "o"])
            if query == "uploadType=multipart" =>
        {
            let content_type = parts.headers["content-type"].to_str().unwrap();
            let (meta, media) = multipart(content_type, &body);
            let name = format!("{bucket}/{}", meta["name"].as_str().unwrap());
            objects.insert(name, (media, meta.clone()));
            Json(json!({"kind": "storage#object", "name": meta["name"], "bucket": bucket}))
                .into_response()
        }
        (Method::GET, ["", "storage", "v1", "b", bucket, "o", object]) if query == "alt=media" => {
            match objects.get(&format!("{bucket}/{object}")) {
                Some((bytes, _)) => bytes.clone().into_response(),
                None => not_found(),
            }
        }
        (
            Method::POST,
            ["", "storage", "v1", "b", bucket, "o", source, "rewriteTo", "b", target, "o", dest],
        ) => {
            let Some(object) = objects.get(&format!("{bucket}/{source}")).cloned() else {
                return not_found();
            };
            // Every first call asks for a second one, as a large or cross-location rewrite does.
            if !query.contains("rewriteToken=step-1") {
                fake.rewrites.fetch_add(1, Ordering::SeqCst);
                return Json(json!({"kind": "storage#rewriteResponse", "done": false, "rewriteToken": "step-1"})).into_response();
            }
            objects.insert(format!("{target}/{dest}"), object);
            Json(json!({"kind": "storage#rewriteResponse", "done": true, "resource": {"name": dest}})).into_response()
        }
        (Method::DELETE, ["", "storage", "v1", "b", bucket, "o", object]) => {
            match objects.remove(&format!("{bucket}/{object}")) {
                Some(_) => StatusCode::NO_CONTENT.into_response(),
                None => not_found(),
            }
        }
        _ => (StatusCode::BAD_REQUEST, format!("unexpected {} {}", parts.method, parts.uri))
            .into_response(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gcs_passes_the_conformance_suite_in_both_layouts() {
    let fake = Arc::new(FakeGcs::default());
    let server =
        FakeServer::start(axum::Router::new().fallback(handle).with_state(fake.clone())).await;
    let store = |bucket: &str, prefix: String| {
        GcsStore::new(bucket, credential(&server.base, TOKEN))
            .with_endpoint(server.base.parse().unwrap())
            .with_prefix(&prefix)
    };
    let next = AtomicUsize::new(0);
    conformance::run_all(|| {
        let n = next.fetch_add(1, Ordering::SeqCst);
        Arc::new(ObjectStoreHost::prefix_pointer(Arc::new(store("site", format!("pointer-{n}")))))
            as Arc<dyn SiteHost>
    })
    .await;
    conformance::run_all(|| {
        let n = next.fetch_add(1, Ordering::SeqCst);
        Arc::new(
            ObjectStoreHost::in_place(
                Arc::new(store("www.example.com", format!("live-{n}"))),
                Arc::new(store("releases", format!("archive-{n}"))),
            )
            .with_cache_control("public, max-age=300"),
        ) as Arc<dyn SiteHost>
    })
    .await;
    assert_eq!(fake.unauthorised.load(Ordering::SeqCst), 0);
    assert!(fake.rewrites.load(Ordering::SeqCst) > 0, "copies ran as rewrites");
    let objects = fake.objects.lock().await;
    let (bytes, meta) = &objects["site/pointer-0/current.pointer"];
    assert!(bytes.starts_with(b"releases/"));
    assert_eq!(meta["cacheControl"], "no-cache, max-age=0");
    let page = objects
        .iter()
        .find(|(k, _)| k.starts_with("www.example.com/") && k.ends_with(".html"))
        .expect("a live page");
    assert_eq!(page.1.1["cacheControl"], "public, max-age=300");
    assert_eq!(page.1.1["contentType"], "text/html; charset=utf-8");
}

/// Runs the conformance suite against a real bucket or `fake-gcs-server`.
///
/// ```sh
/// ADK_SITE_GCS_BUCKET=my-test-bucket ADK_SITE_GCS_TOKEN="$(gcloud auth print-access-token)" \
///   cargo nextest run -p adk-site --features host-gcs --run-ignored only gcs_bucket
/// ```
///
/// `ADK_SITE_GCS_ENDPOINT` points at an emulator such as `fake-gcs-server`
/// (`http://127.0.0.1:4443`). Objects go under `adk-site-conformance/<run>/`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a Cloud Storage bucket: set ADK_SITE_GCS_BUCKET and ADK_SITE_GCS_TOKEN"]
async fn gcs_bucket_passes_the_conformance_suite() {
    adk_core::ensure_crypto_provider();
    let bucket = std::env::var("ADK_SITE_GCS_BUCKET").expect("set ADK_SITE_GCS_BUCKET");
    let token = std::env::var("ADK_SITE_GCS_TOKEN").expect("set ADK_SITE_GCS_TOKEN");
    let endpoint = std::env::var("ADK_SITE_GCS_ENDPOINT")
        .unwrap_or_else(|_| "https://storage.googleapis.com".into());
    let run = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let next = AtomicUsize::new(0);
    conformance::run_all(|| {
        let n = next.fetch_add(1, Ordering::SeqCst);
        let store = GcsStore::new(bucket.clone(), credential(&endpoint, &token))
            .with_endpoint(endpoint.parse().unwrap())
            .with_prefix(&format!("adk-site-conformance/{run}/{n}"));
        Arc::new(ObjectStoreHost::prefix_pointer(Arc::new(store))) as Arc<dyn SiteHost>
    })
    .await;
}
