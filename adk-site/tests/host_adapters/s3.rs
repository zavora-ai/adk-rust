//! `S3Store` against a fake S3 that checks every Signature Version 4 signature itself, and
//! against a real S3-compatible store (MinIO) when one is configured.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_site::host::conformance;
use adk_site::host::object::{CachePurge, ObjectStoreHost};
use adk_site::host::s3::{CloudFrontInvalidation, S3Store};
use adk_site::host::{HostCredential, SiteHost};
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use hmac::{Hmac, Mac};
use sha2::{Digest as _, Sha256};

use crate::support::{FakeServer, credential, decode};

pub const KEYS: &str = r#"{"accessKeyId":"AKIDFAKE","secretAccessKey":"fake/secret+key"}"#;
const SECRET: &str = "fake/secret+key";

type Objects = BTreeMap<String, (Bytes, Option<String>, Option<String>)>;

/// An S3 subset: path-style object GET, PUT, copy and DELETE, and CloudFront
/// invalidations, each refused unless its Signature Version 4 verifies.
#[derive(Default)]
pub struct FakeS3 {
    pub objects: tokio::sync::Mutex<Objects>,
    pub invalidations: tokio::sync::Mutex<Vec<String>>,
    pub refused: AtomicUsize,
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// Recomputes the signature from the request as received.
fn verify(method: &Method, uri: &Uri, headers: &HeaderMap, body: &[u8]) -> Result<(), String> {
    let header = |name: &str| {
        headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string()
    };
    let auth = header("authorization");
    let rest = auth.strip_prefix("AWS4-HMAC-SHA256 ").ok_or("not SigV4")?;
    let mut parts = BTreeMap::new();
    for part in rest.split(", ") {
        let (k, v) = part.split_once('=').ok_or("bad authorization")?;
        parts.insert(k, v);
    }
    let credential: Vec<&str> = parts["Credential"].split('/').collect();
    let [_, date, region, service, "aws4_request"] = credential.as_slice() else {
        return Err("bad credential scope".into());
    };
    let payload = header("x-amz-content-sha256");
    if payload != hex::encode(Sha256::digest(body)) {
        return Err("payload hash mismatch".into());
    }
    let signed = parts["SignedHeaders"];
    let canonical_headers: String =
        signed.split(';').map(|name| format!("{name}:{}\n", header(name).trim())).collect();
    let mut query: Vec<&str> = uri.query().map(|q| q.split('&').collect()).unwrap_or_default();
    query.sort_unstable();
    let canonical = format!(
        "{method}\n{}\n{}\n{canonical_headers}\n{signed}\n{payload}",
        uri.path(),
        query.join("&")
    );
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{date}/{region}/{service}/aws4_request\n{}",
        header("x-amz-date"),
        hex::encode(Sha256::digest(canonical.as_bytes()))
    );
    let key = [*date, region, service, "aws4_request"]
        .iter()
        .fold(format!("AWS4{SECRET}").into_bytes(), |key, part| hmac(&key, part));
    if hex::encode(hmac(&key, &to_sign)) == parts["Signature"] {
        Ok(())
    } else {
        Err("SignatureDoesNotMatch".into())
    }
}

fn error(status: StatusCode, code: &str) -> Response {
    (status, format!("<?xml version=\"1.0\"?><Error><Code>{code}</Code></Error>")).into_response()
}

async fn handle(State(fake): State<Arc<FakeS3>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    if let Err(why) = verify(&parts.method, &parts.uri, &parts.headers, &body) {
        fake.refused.fetch_add(1, Ordering::SeqCst);
        return error(StatusCode::FORBIDDEN, &why);
    }
    let path = decode(parts.uri.path());
    if path.starts_with("/2020-05-31/distribution/") && parts.method == Method::POST {
        fake.invalidations.lock().await.push(String::from_utf8_lossy(&body).into_owned());
        return (StatusCode::CREATED, "<Invalidation><Id>I1</Id></Invalidation>").into_response();
    }
    let key = path.trim_start_matches('/').to_string();
    let header =
        |name: &str| parts.headers.get(name).and_then(|v| v.to_str().ok()).map(String::from);
    let mut objects = fake.objects.lock().await;
    match parts.method {
        Method::GET => match objects.get(&key) {
            Some((bytes, content_type, _)) => {
                ([("content-type", content_type.clone().unwrap_or_default())], bytes.clone())
                    .into_response()
            }
            None => error(StatusCode::NOT_FOUND, "NoSuchKey"),
        },
        Method::PUT => {
            if let Some(source) = header("x-amz-copy-source") {
                let Some(object) = objects.get(decode(&source).trim_start_matches('/')).cloned()
                else {
                    return error(StatusCode::NOT_FOUND, "NoSuchKey");
                };
                objects.insert(key, object);
                return "<CopyObjectResult><ETag>\"e\"</ETag></CopyObjectResult>".into_response();
            }
            objects.insert(key, (body, header("content-type"), header("cache-control")));
            StatusCode::OK.into_response()
        }
        Method::DELETE => {
            objects.remove(&key);
            StatusCode::NO_CONTENT.into_response()
        }
        _ => error(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed"),
    }
}

pub async fn start() -> (FakeServer, Arc<FakeS3>) {
    let fake = Arc::new(FakeS3::default());
    let router = axum::Router::new().fallback(handle).with_state(fake.clone());
    (FakeServer::start(router).await, fake)
}

pub fn store(server: &FakeServer, bucket: &str, prefix: &str) -> S3Store {
    S3Store::new(
        server.base.parse().unwrap(),
        "us-east-1",
        bucket,
        credential(&format!("{}/{bucket}", server.base), KEYS),
    )
    .path_style(true)
    .with_prefix(prefix)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s3_prefix_pointer_passes_the_conformance_suite() {
    let (server, fake) = start().await;
    let next = AtomicUsize::new(0);
    conformance::run_all(|| {
        let prefix = format!("site-{}", next.fetch_add(1, Ordering::SeqCst));
        Arc::new(ObjectStoreHost::prefix_pointer(Arc::new(store(&server, "site", &prefix))))
            as Arc<dyn SiteHost>
    })
    .await;
    assert_eq!(fake.refused.load(Ordering::SeqCst), 0, "every request was signed correctly");
    let objects = fake.objects.lock().await;
    let pointer = &objects["site/site-0/current.pointer"];
    assert_eq!(pointer.2.as_deref(), Some("no-cache, max-age=0"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s3_website_in_place_passes_the_conformance_suite() {
    let (server, fake) = start().await;
    let next = AtomicUsize::new(0);
    conformance::run_all(|| {
        let n = next.fetch_add(1, Ordering::SeqCst);
        Arc::new(ObjectStoreHost::in_place(
            Arc::new(store(&server, "www", &format!("site-{n}"))),
            Arc::new(store(&server, "releases", &format!("site-{n}"))),
        )) as Arc<dyn SiteHost>
    })
    .await;
    assert_eq!(fake.refused.load(Ordering::SeqCst), 0);
    let objects = fake.objects.lock().await;
    assert!(
        objects.contains_key("www/site-0/index.html")
            || objects.keys().any(|k| k.starts_with("www/site-0/p"))
    );
    assert!(objects.keys().filter(|k| k.starts_with("www/")).all(|k| !k.contains("/releases/1/")));
}

#[tokio::test]
async fn a_wrong_secret_is_refused_and_reported_without_the_secret() {
    let (server, fake) = start().await;
    let wrong = r#"{"accessKeyId":"AKIDFAKE","secretAccessKey":"not-the-secret"}"#;
    let store = S3Store::new(
        server.base.parse().unwrap(),
        "us-east-1",
        "site",
        credential(&format!("{}/site", server.base), wrong),
    )
    .path_style(true);
    let error = adk_site::host::object::ObjectStore::get(&store, "index.html").await.unwrap_err();
    let text = error.to_string();
    assert!(text.contains("403"), "{text}");
    assert!(!text.contains("not-the-secret"), "{text}");
    assert_eq!(fake.refused.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_credential_for_another_bucket_is_not_sent() {
    let (server, fake) = start().await;
    let store = S3Store::new(
        server.base.parse().unwrap(),
        "us-east-1",
        "site",
        credential(&format!("{}/other", server.base), KEYS),
    )
    .path_style(true);
    let error = adk_site::host::object::ObjectStore::get(&store, "index.html").await.unwrap_err();
    assert!(error.to_string().contains("credential not usable"), "{error}");
    assert_eq!(fake.objects.lock().await.len(), 0);
    assert_eq!(fake.refused.load(Ordering::SeqCst), 0, "nothing reached the server");
}

#[tokio::test]
async fn cloudfront_invalidations_are_signed_and_name_directory_urls() {
    let (server, fake) = start().await;
    let purge = CloudFrontInvalidation::new("E2EXAMPLE", credential(&server.base, KEYS))
        .with_endpoint(server.base.parse().unwrap());
    purge.purge(&["/a/index.html".into(), "/b c.css".into()]).await.unwrap();
    let batches = fake.invalidations.lock().await;
    assert_eq!(fake.refused.load(Ordering::SeqCst), 0);
    let xml = &batches[0];
    assert!(xml.contains("<Quantity>3</Quantity>"), "{xml}");
    for path in ["<Path>/a/</Path>", "<Path>/a/index.html</Path>", "<Path>/b%20c.css</Path>"] {
        assert!(xml.contains(path), "{path} in {xml}");
    }
    drop(batches);
    let many: Vec<String> = (0..20).map(|i| format!("/p{i}.html")).collect();
    let purge = CloudFrontInvalidation::new("E2EXAMPLE", credential(&server.base, KEYS))
        .with_endpoint(server.base.parse().unwrap())
        .with_max_paths(10);
    purge.purge(&many).await.unwrap();
    assert!(fake.invalidations.lock().await[1].contains("<Items><Path>/*</Path></Items>"));
}

/// Runs the conformance suite against a real S3-compatible store.
///
/// With MinIO (`brew install minio` or the `minio/minio` image):
///
/// ```sh
/// MINIO_ROOT_USER=adk MINIO_ROOT_PASSWORD=adk-secret minio server /tmp/minio --address 127.0.0.1:9000 &
/// curl -X PUT --user adk:adk-secret --aws-sigv4 aws:amz:us-east-1:s3 http://127.0.0.1:9000/adk-site
/// ADK_SITE_S3_ENDPOINT=http://127.0.0.1:9000 ADK_SITE_S3_BUCKET=adk-site \
/// ADK_SITE_S3_KEYS='{"accessKeyId":"adk","secretAccessKey":"adk-secret"}' \
///   cargo nextest run -p adk-site --features host-s3 --run-ignored only s3_compatible_store
/// ```
///
/// `ADK_SITE_S3_REGION` (default `us-east-1`) and `ADK_SITE_S3_PATH_STYLE=false` select
/// Amazon S3's virtual-hosted addressing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs an S3-compatible store: set ADK_SITE_S3_ENDPOINT, ADK_SITE_S3_BUCKET and ADK_SITE_S3_KEYS"]
async fn s3_compatible_store_passes_the_conformance_suite() {
    adk_core::ensure_crypto_provider();
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
    let endpoint: adk_core::Url = var("ADK_SITE_S3_ENDPOINT").parse().unwrap();
    let bucket = var("ADK_SITE_S3_BUCKET");
    let region = std::env::var("ADK_SITE_S3_REGION").unwrap_or_else(|_| "us-east-1".into());
    let path_style = std::env::var("ADK_SITE_S3_PATH_STYLE").map_or(true, |v| v != "false");
    let keys = var("ADK_SITE_S3_KEYS");
    let run = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let make = |name: &str| -> S3Store {
        let probe = S3Store::new(
            endpoint.clone(),
            region.clone(),
            bucket.clone(),
            credential(endpoint.as_str(), &keys),
        )
        .path_style(path_style);
        let resource: HostCredential = credential(probe.bucket_url().as_str(), &keys);
        S3Store::new(endpoint.clone(), region.clone(), bucket.clone(), resource)
            .path_style(path_style)
            .with_prefix(&format!("adk-site-conformance/{run}/{name}"))
    };
    let next = AtomicUsize::new(0);
    conformance::run_all(|| {
        let n = next.fetch_add(1, Ordering::SeqCst);
        Arc::new(ObjectStoreHost::prefix_pointer(Arc::new(make(&format!("pointer-{n}")))))
            as Arc<dyn SiteHost>
    })
    .await;
    conformance::run_all(|| {
        let n = next.fetch_add(1, Ordering::SeqCst);
        Arc::new(ObjectStoreHost::in_place(
            Arc::new(make(&format!("live-{n}"))),
            Arc::new(make(&format!("archive-{n}"))),
        )) as Arc<dyn SiteHost>
    })
    .await;
}
