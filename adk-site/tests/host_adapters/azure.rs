//! `AzureBlobStore` against a fake Blob service that checks every Shared Key signature
//! itself and answers copies as pending first, and against Azurite or a real account when
//! one is configured.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use adk_site::host::SiteHost;
use adk_site::host::azure::{AzureAuth, AzureBlobStore};
use adk_site::host::conformance;
use adk_site::host::object::ObjectStoreHost;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::support::{FakeServer, credential, decode};

const ACCOUNT: &str = "siteaccount";
// Not a real account key: 32 bytes of 0x2a, base64.
const KEY: &str = "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio=";

/// Blob name to its bytes, content type and cache control.
type Blobs = BTreeMap<String, (Bytes, Option<String>, Option<String>)>;

#[derive(Default)]
struct FakeBlob {
    blobs: tokio::sync::Mutex<Blobs>,
    pending: tokio::sync::Mutex<BTreeMap<String, usize>>,
    refused: AtomicUsize,
    copies: AtomicUsize,
}

/// Rebuilds the Shared Key string-to-sign from the request as received.
fn verify(method: &Method, uri: &Uri, headers: &HeaderMap) -> bool {
    let header = |name: &str| {
        headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string()
    };
    let length = match header("content-length").as_str() {
        "0" => String::new(),
        other => other.to_string(),
    };
    let mut ms: Vec<(String, String)> = headers
        .iter()
        .filter(|(k, _)| k.as_str().starts_with("x-ms-"))
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().trim().to_string()))
        .collect();
    ms.sort();
    let canonical: String = ms.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let to_sign = format!(
        "{method}\n{}\n{}\n{length}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{canonical}/{ACCOUNT}{}",
        header("content-encoding"),
        header("content-language"),
        header("content-md5"),
        header("content-type"),
        header("date"),
        header("if-modified-since"),
        header("if-match"),
        header("if-none-match"),
        header("if-unmodified-since"),
        header("range"),
        uri.path()
    );
    let mut mac = Hmac::<Sha256>::new_from_slice(&STANDARD.decode(KEY).unwrap()).unwrap();
    mac.update(to_sign.as_bytes());
    header("authorization")
        == format!("SharedKey {ACCOUNT}:{}", STANDARD.encode(mac.finalize().into_bytes()))
}

fn blob_error(status: StatusCode, code: &str) -> Response {
    (
        status,
        [("x-ms-error-code", code.to_string())],
        format!("<?xml version=\"1.0\"?><Error><Code>{code}</Code></Error>"),
    )
        .into_response()
}

async fn handle(State(fake): State<Arc<FakeBlob>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    if !verify(&parts.method, &parts.uri, &parts.headers) {
        fake.refused.fetch_add(1, Ordering::SeqCst);
        return blob_error(StatusCode::FORBIDDEN, "AuthenticationFailed");
    }
    // Path style: /<account>/<container>/<blob>.
    let path = decode(parts.uri.path());
    let key = path.trim_start_matches(&format!("/{ACCOUNT}/")).to_string();
    let header =
        |name: &str| parts.headers.get(name).and_then(|v| v.to_str().ok()).map(String::from);
    let mut blobs = fake.blobs.lock().await;
    match parts.method {
        Method::GET => match blobs.get(&key) {
            Some((bytes, content_type, _)) => {
                ([("content-type", content_type.clone().unwrap_or_default())], bytes.clone())
                    .into_response()
            }
            None => blob_error(StatusCode::NOT_FOUND, "BlobNotFound"),
        },
        Method::HEAD => {
            let mut pending = fake.pending.lock().await;
            let status = match pending.get_mut(&key) {
                Some(0) | None => "success",
                Some(left) => {
                    *left -= 1;
                    "pending"
                }
            };
            (StatusCode::OK, [("x-ms-copy-status", status)]).into_response()
        }
        Method::PUT => {
            if let Some(source) = header("x-ms-copy-source") {
                let source: adk_core::Url = source.parse().unwrap();
                let from =
                    decode(source.path()).trim_start_matches(&format!("/{ACCOUNT}/")).to_string();
                let Some(blob) = blobs.get(&from).cloned() else {
                    return blob_error(StatusCode::NOT_FOUND, "CannotVerifyCopySource");
                };
                blobs.insert(key.clone(), blob);
                fake.copies.fetch_add(1, Ordering::SeqCst);
                fake.pending.lock().await.insert(key, 1);
                return (StatusCode::ACCEPTED, [("x-ms-copy-status", "pending")]).into_response();
            }
            if header("x-ms-blob-type").as_deref() != Some("BlockBlob") {
                return blob_error(StatusCode::BAD_REQUEST, "MissingRequiredHeader");
            }
            blobs.insert(key, (body, header("content-type"), header("x-ms-blob-cache-control")));
            StatusCode::CREATED.into_response()
        }
        Method::DELETE => match blobs.remove(&key) {
            Some(_) => StatusCode::ACCEPTED.into_response(),
            None => blob_error(StatusCode::NOT_FOUND, "BlobNotFound"),
        },
        _ => blob_error(StatusCode::METHOD_NOT_ALLOWED, "UnsupportedHttpVerb"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn azure_static_website_passes_the_conformance_suite() {
    let fake = Arc::new(FakeBlob::default());
    let server =
        FakeServer::start(axum::Router::new().fallback(handle).with_state(fake.clone())).await;
    let endpoint: adk_core::Url = format!("{}/{ACCOUNT}", server.base).parse().unwrap();
    let store = |container: &str, prefix: String| {
        AzureBlobStore::new(
            ACCOUNT,
            container,
            AzureAuth::SharedKey,
            credential(endpoint.as_str(), KEY),
        )
        .with_endpoint(endpoint.clone())
        .with_prefix(&prefix)
    };
    let next = AtomicUsize::new(0);
    conformance::run_all(|| {
        let n = next.fetch_add(1, Ordering::SeqCst);
        let website = AzureBlobStore::static_website(
            ACCOUNT,
            AzureAuth::SharedKey,
            credential(endpoint.as_str(), KEY),
        )
        .with_endpoint(endpoint.clone())
        .with_prefix(&format!("site-{n}"));
        Arc::new(ObjectStoreHost::in_place(
            Arc::new(website),
            Arc::new(store("releases", format!("site-{n}"))),
        )) as Arc<dyn SiteHost>
    })
    .await;
    conformance::run_all(|| {
        let n = next.fetch_add(1, Ordering::SeqCst);
        Arc::new(ObjectStoreHost::prefix_pointer(Arc::new(store("site", format!("pointer-{n}")))))
            as Arc<dyn SiteHost>
    })
    .await;
    assert_eq!(fake.refused.load(Ordering::SeqCst), 0, "every Shared Key signature verified");
    assert!(fake.copies.load(Ordering::SeqCst) > 0, "pending copies were awaited");
    let blobs = fake.blobs.lock().await;
    assert!(
        blobs.keys().any(|k| k.starts_with("$web/site-0/")),
        "the static website is written in place"
    );
    assert!(
        blobs
            .keys()
            .filter(|k| k.starts_with("$web/"))
            .all(|k| !k.contains("/releases/1/") && !k.ends_with(".pointer"))
    );
}

#[tokio::test]
async fn a_wrong_account_key_is_refused() {
    let fake = Arc::new(FakeBlob::default());
    let server =
        FakeServer::start(axum::Router::new().fallback(handle).with_state(fake.clone())).await;
    let endpoint: adk_core::Url = format!("{}/{ACCOUNT}", server.base).parse().unwrap();
    let wrong = STANDARD.encode([7u8; 32]);
    let store = AzureBlobStore::new(
        ACCOUNT,
        "site",
        AzureAuth::SharedKey,
        credential(endpoint.as_str(), &wrong),
    )
    .with_endpoint(endpoint);
    let error = adk_site::host::object::ObjectStore::get(&store, "index.html").await.unwrap_err();
    assert!(error.to_string().contains("403"), "{error}");
    assert!(!error.to_string().contains(&wrong));
    let not_base64 = AzureBlobStore::new(
        ACCOUNT,
        "site",
        AzureAuth::SharedKey,
        credential(&server.base, "not base64!"),
    )
    .with_endpoint(format!("{}/{ACCOUNT}", server.base).parse().unwrap());
    let error =
        adk_site::host::object::ObjectStore::get(&not_base64, "index.html").await.unwrap_err();
    assert!(error.to_string().contains("not a base64 account key"), "{error}");
}

/// Runs the conformance suite against Azurite or a storage account.
///
/// With Azurite (`npx azurite-blob --blobHost 127.0.0.1 --location /tmp/azurite`), whose
/// published development key is not a secret:
///
/// ```sh
/// ADK_SITE_AZURE_ENDPOINT=http://127.0.0.1:10000/devstoreaccount1 ADK_SITE_AZURE_ACCOUNT=devstoreaccount1 \
/// ADK_SITE_AZURE_KEY=Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw== \
///   cargo nextest run -p adk-site --features host-azure --run-ignored only azure_storage
/// ```
///
/// The containers named by `ADK_SITE_AZURE_CONTAINER` (default `site`) and
/// `ADK_SITE_AZURE_ARCHIVE` (default `site-releases`) must exist.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs Azurite or a storage account: set ADK_SITE_AZURE_ACCOUNT and ADK_SITE_AZURE_KEY"]
async fn azure_storage_passes_the_conformance_suite() {
    adk_core::ensure_crypto_provider();
    let account = std::env::var("ADK_SITE_AZURE_ACCOUNT").expect("set ADK_SITE_AZURE_ACCOUNT");
    let key = std::env::var("ADK_SITE_AZURE_KEY").expect("set ADK_SITE_AZURE_KEY");
    let endpoint: adk_core::Url = std::env::var("ADK_SITE_AZURE_ENDPOINT")
        .unwrap_or_else(|_| format!("https://{account}.blob.core.windows.net"))
        .parse()
        .unwrap();
    let container = std::env::var("ADK_SITE_AZURE_CONTAINER").unwrap_or_else(|_| "site".into());
    let archive =
        std::env::var("ADK_SITE_AZURE_ARCHIVE").unwrap_or_else(|_| "site-releases".into());
    let run = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let store = |container: &str, prefix: String| {
        AzureBlobStore::new(
            account.clone(),
            container,
            AzureAuth::SharedKey,
            credential(endpoint.as_str(), &key),
        )
        .with_endpoint(endpoint.clone())
        .with_prefix(&prefix)
    };
    let next = AtomicUsize::new(0);
    conformance::run_all(|| {
        let n = next.fetch_add(1, Ordering::SeqCst);
        Arc::new(ObjectStoreHost::in_place(
            Arc::new(store(&container, format!("adk-site-conformance/{run}/{n}"))),
            Arc::new(store(&archive, format!("adk-site-conformance/{run}/{n}"))),
        )) as Arc<dyn SiteHost>
    })
    .await;
    conformance::run_all(|| {
        let n = next.fetch_add(1, Ordering::SeqCst);
        Arc::new(ObjectStoreHost::prefix_pointer(Arc::new(store(
            &container,
            format!("adk-site-conformance/{run}/p{n}"),
        )))) as Arc<dyn SiteHost>
    })
    .await;
}
