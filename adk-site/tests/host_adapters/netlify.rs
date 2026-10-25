//! `NetlifyHost` against a fake of Netlify's file-digest deploy API: digests it already
//! holds are not required, a deploy goes live whole when its last required file arrives,
//! and a site with locked auto-publishing needs a restore.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use adk_site::host::conformance::{self, Fixture};
use adk_site::host::netlify::NetlifyHost;
use adk_site::host::{HostTarget, SiteHost};
use adk_site::redirects::RedirectsFormat;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use sha1::{Digest as _, Sha1};

use crate::support::{FakeServer, credential, decode};

const TOKEN: &str = "nfp_fake_token";

#[derive(Clone)]
struct FakeDeploy {
    id: String,
    title: String,
    draft: bool,
    files: BTreeMap<String, String>,
    required: BTreeSet<String>,
}

#[derive(Default)]
struct Netlify {
    base: std::sync::OnceLock<String>,
    blobs: tokio::sync::Mutex<BTreeMap<String, Bytes>>,
    deploys: tokio::sync::Mutex<Vec<FakeDeploy>>,
    published: tokio::sync::RwLock<Option<String>>,
    locked: AtomicBool,
    uploads: AtomicUsize,
    restores: AtomicUsize,
}

impl Netlify {
    fn base(&self) -> &str {
        self.base.get().expect("base")
    }

    fn deploy_json(&self, deploy: &FakeDeploy) -> Value {
        json!({
            "id": deploy.id,
            "site_id": "site-1",
            "title": deploy.title,
            "state": if deploy.required.is_empty() { "ready" } else { "uploading" },
            "required": deploy.required,
            "deploy_ssl_url": format!("{}/__deploy/{}/", self.base(), deploy.id),
            "context": if deploy.draft { "deploy-preview" } else { "production" },
        })
    }

    async fn ready(&self, deploy: &FakeDeploy) {
        if deploy.required.is_empty() && !deploy.draft && !self.locked.load(Ordering::SeqCst) {
            *self.published.write().await = Some(deploy.id.clone());
        }
    }

    async fn serve(&self, deploy_id: Option<&str>, path: &str) -> Response {
        let id = match deploy_id {
            Some(id) => Some(id.to_string()),
            None => self.published.read().await.clone(),
        };
        let deploys = self.deploys.lock().await;
        let Some(deploy) = id.and_then(|id| deploys.iter().find(|d| d.id == id).cloned()) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let mut path = format!("/{}", path.trim_start_matches('/'));
        if path.ends_with('/') {
            path.push_str("index.html");
        }
        match deploy.files.get(&path) {
            Some(sha) => self.blobs.lock().await[sha].clone().into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }
}

fn page<T: Clone>(items: &[T], query: &str) -> Vec<T> {
    let param = |name: &str| {
        query
            .split('&')
            .find_map(|p| p.strip_prefix(&format!("{name}=")))
            .and_then(|v| v.parse::<usize>().ok())
    };
    let (page, per_page) = (param("page").unwrap_or(1), param("per_page").unwrap_or(100));
    items.iter().skip((page - 1) * per_page).take(per_page).cloned().collect()
}

async fn handle(State(fake): State<Arc<Netlify>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    let path = parts.uri.path().to_string();
    let query = parts.uri.query().unwrap_or_default().to_string();
    if let Some(rest) = path.strip_prefix("/__site/") {
        return fake.serve(None, &decode(rest)).await;
    }
    if let Some(rest) = path.strip_prefix("/__deploy/") {
        let (id, file) = rest.split_once('/').unwrap_or((rest, ""));
        return fake.serve(Some(id), &decode(file)).await;
    }
    if parts.headers.get("authorization").and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {TOKEN}"))
    {
        return (StatusCode::UNAUTHORIZED, Json(json!({"code": 401, "message": "Access Denied"})))
            .into_response();
    }
    let api_path = path.trim_start_matches("/api/v1/");
    let parts_limit = if api_path.starts_with("deploys/") { 4 } else { 5 };
    let segments: Vec<&str> = api_path.splitn(parts_limit, '/').collect();
    match (parts.method.clone(), segments.as_slice()) {
        (Method::GET, ["sites", "site-1"]) => {
            let published = fake.published.read().await.clone();
            Json(json!({
                "id": "site-1",
                "name": "example",
                "ssl_url": format!("{}/__site/", fake.base()),
                "published_deploy": published.map(|id| json!({"id": id})),
            }))
            .into_response()
        }
        (Method::POST, ["sites", "site-1", "deploys"]) => {
            let request: Value = serde_json::from_slice(&body).unwrap();
            let files: BTreeMap<String, String> =
                serde_json::from_value(request["files"].clone()).unwrap();
            let blobs = fake.blobs.lock().await;
            let required =
                files.values().filter(|sha| !blobs.contains_key(*sha)).cloned().collect();
            drop(blobs);
            let title = query
                .split('&')
                .find_map(|p| p.strip_prefix("title="))
                .map(decode)
                .unwrap_or_default();
            let mut deploys = fake.deploys.lock().await;
            let deploy = FakeDeploy {
                id: format!("d{}", deploys.len() + 1),
                title,
                draft: request["draft"].as_bool().unwrap_or(false),
                files,
                required,
            };
            deploys.insert(0, deploy.clone());
            drop(deploys);
            fake.ready(&deploy).await;
            Json(fake.deploy_json(&deploy)).into_response()
        }
        (Method::GET, ["sites", "site-1", "deploys"]) => {
            let deploys = fake.deploys.lock().await.clone();
            let list: Vec<Value> = deploys.iter().map(|d| fake.deploy_json(d)).collect();
            Json(page(&list, &query)).into_response()
        }
        (Method::POST, ["sites", "site-1", "deploys", id, "restore"]) => {
            fake.restores.fetch_add(1, Ordering::SeqCst);
            *fake.published.write().await = Some((*id).to_string());
            Json(json!({"id": id, "state": "current"})).into_response()
        }
        (Method::GET, ["deploys", id]) => {
            let deploys = fake.deploys.lock().await;
            match deploys.iter().find(|d| d.id == *id) {
                Some(deploy) => Json(fake.deploy_json(deploy)).into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
        (Method::GET, ["deploys", id, "files"]) => {
            let deploys = fake.deploys.lock().await;
            let Some(deploy) = deploys.iter().find(|d| d.id == *id) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let files: Vec<Value> = deploy
                .files
                .iter()
                .map(|(p, sha)| json!({"id": p, "path": p, "sha": sha, "mime_type": "", "size": 0}))
                .collect();
            Json(page(&files, &query)).into_response()
        }
        (Method::PUT, ["deploys", id, "files", file]) => {
            let sha = hex::encode(Sha1::digest(&body));
            let mut deploys = fake.deploys.lock().await;
            let Some(deploy) = deploys.iter_mut().find(|d| d.id == *id) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            if deploy.files.get(&format!("/{}", decode(file))) != Some(&sha)
                || !deploy.required.remove(&sha)
            {
                return (StatusCode::UNPROCESSABLE_ENTITY, "file not required").into_response();
            }
            fake.uploads.fetch_add(1, Ordering::SeqCst);
            fake.blobs.lock().await.insert(sha, body);
            let deploy = deploy.clone();
            drop(deploys);
            fake.ready(&deploy).await;
            Json(json!({"id": file, "path": file})).into_response()
        }
        _ => (StatusCode::NOT_FOUND, format!("unexpected {} {}", parts.method, parts.uri))
            .into_response(),
    }
}

async fn start() -> (FakeServer, Arc<Netlify>) {
    let fake = Arc::new(Netlify::default());
    let server =
        FakeServer::start(axum::Router::new().fallback(handle).with_state(fake.clone())).await;
    fake.base.set(server.base.clone()).ok();
    (server, fake)
}

fn host(server: &FakeServer) -> NetlifyHost {
    NetlifyHost::new("site-1", credential(&format!("{}/api/v1/", server.base), TOKEN))
        .with_api(format!("{}/api/v1/", server.base).parse().unwrap())
        .with_polling(Duration::from_millis(5), Duration::from_secs(10))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn netlify_passes_the_conformance_suite() {
    // One fresh site per check, with room for checks the suite gains.
    let mut made = Vec::new();
    for _ in 0..8 {
        made.push(start().await);
    }
    let next = AtomicUsize::new(0);
    conformance::run_all(|| {
        let (server, _) = &made[next.fetch_add(1, Ordering::SeqCst)];
        Arc::new(host(server)) as Arc<dyn SiteHost>
    })
    .await;
    let restores: usize = made.iter().map(|(_, f)| f.restores.load(Ordering::SeqCst)).sum();
    assert_eq!(restores, 0, "auto-publishing needed no restore");
}

#[tokio::test]
async fn unchanged_files_are_not_uploaded_and_previews_are_drafts() -> adk_site::Result<()> {
    let (server, fake) = start().await;
    let host = host(&server);
    assert_eq!(host.redirects_format(), RedirectsFormat::Netlify);
    let mut fixture = Fixture::new().await;
    let files = |v: &str| {
        vec![
            ("/index.html", format!("<p>{v}</p>").into_bytes()),
            ("/about/index.html", b"<p>about</p>".to_vec()),
        ]
    };
    let a = fixture.release(&files("a"), Vec::new(), false).await;
    host.publish(&a, HostTarget::Production).await?;
    let b = fixture.release(&files("b"), Vec::new(), false).await;
    let report = host.publish(&b, HostTarget::Production).await?;
    let content: Vec<&String> =
        report.uploaded.iter().filter(|p| !adk_site::release::is_log_path(p)).collect();
    assert_eq!(content, vec!["/index.html"], "Netlify already holds /about/");
    let preview = fixture.release(&files("c"), Vec::new(), false).await;
    host.publish(&preview, HostTarget::Preview).await?;
    assert_eq!(
        host.fetch("/").await?.as_deref(),
        Some(b"<p>b</p>".as_slice()),
        "a draft is not published"
    );
    assert!(fake.deploys.lock().await[0].draft);
    Ok(())
}

#[tokio::test]
async fn a_locked_site_is_published_by_restore() -> adk_site::Result<()> {
    let (server, fake) = start().await;
    fake.locked.store(true, Ordering::SeqCst);
    let host = host(&server);
    let mut fixture = Fixture::new().await;
    let a = fixture.release(&[("/index.html", b"<p>a</p>".to_vec())], Vec::new(), false).await;
    host.publish(&a, HostTarget::Production).await?;
    assert_eq!(fake.restores.load(Ordering::SeqCst), 1);
    assert_eq!(host.fetch("/").await?.as_deref(), Some(b"<p>a</p>".as_slice()));
    Ok(())
}

/// Runs the conformance suite against real Netlify sites, one new site per check, each
/// deleted afterwards.
///
/// ```sh
/// ADK_SITE_NETLIFY_TOKEN=<personal access token> \
///   cargo nextest run -p adk-site --features host-netlify --run-ignored only netlify_sites
/// ```
///
/// The suite makes about a dozen deploys; it waits between checks to stay inside
/// Netlify's limit of 3 deploys a minute.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a Netlify account: set ADK_SITE_NETLIFY_TOKEN"]
async fn netlify_sites_pass_the_conformance_suite() {
    adk_core::ensure_crypto_provider();
    let token = std::env::var("ADK_SITE_NETLIFY_TOKEN").expect("set ADK_SITE_NETLIFY_TOKEN");
    let api = "https://api.netlify.com/api/v1";
    let client = reqwest::Client::new();
    let mut sites = Vec::new();
    for _ in 0..6 {
        let site: Value = client
            .post(format!("{api}/sites"))
            .bearer_auth(&token)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        sites.push(site["id"].as_str().expect("site id").to_string());
    }
    let result = tokio::spawn({
        let (sites, token) = (sites.clone(), token.clone());
        async move {
            let make = |n: usize| {
                Arc::new(NetlifyHost::new(sites[n].clone(), credential(&format!("{api}/"), &token)))
                    as Arc<dyn SiteHost>
            };
            conformance::checkpoint_last(make(0).as_ref()).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
            conformance::atomic_under_concurrent_reads(make(1)).await;
            tokio::time::sleep(Duration::from_secs(45)).await;
            conformance::atomic_delta_under_concurrent_reads(make(5)).await;
            tokio::time::sleep(Duration::from_secs(45)).await;
            conformance::exact_rollback(make(2).as_ref()).await;
            tokio::time::sleep(Duration::from_secs(60)).await;
            conformance::redirect_round_trip(make(3).as_ref()).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
            conformance::delta_publication(make(4).as_ref()).await;
        }
    })
    .await;
    for site in &sites {
        let _ = client.delete(format!("{api}/sites/{site}")).bearer_auth(&token).send().await;
    }
    result.expect("the conformance suite passed on Netlify");
}
