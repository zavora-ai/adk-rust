//! Shared fixtures of the migration tests (spec 3.5 §7.8, §11): an in-process server for the
//! recorded WordPress site and its `infected` overlay, source contexts, a credential broker
//! and a container driver that replays without Docker.

// Each test binary uses a subset of these helpers.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_auth::web_bot_auth::{Ed25519SigningKey, HttpSignatureSigner};
use adk_core::{
    ActingContext, CredentialBroker, Result as AdkResult, RevocationScope, ScopedToken, SubjectKind,
};
use adk_site::migrate::claim::{DomainClaim, RecordKind, StaticDnsResolver};
use adk_site::migrate::http::{CrawlSigner, EgressScope, HttpClient};
use adk_site::migrate::sources::{ContainerDriver, ReplayContainer, ReplaySpec};
use adk_site::migrate::{ClaimScope, ProvenDomain, SourceContext};
use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Method, Response, Uri};
use base64::Engine as _;
use url::Url;

/// The legacy site's public origin.
pub const ORIGIN: &str = "https://solar.test";
/// Its domain.
pub const DOMAIN: &str = "solar.test";
/// The application password the broker issues; the fixture server accepts only it.
pub const APP_PASSWORD: &str = "owner:abcd efgh ijkl mnop";

pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/migrate")
}

/// One request the fixture server answered.
#[derive(Debug, Clone)]
pub struct Seen {
    pub path: String,
    pub user_agent: String,
    pub signed: bool,
}

#[derive(Debug, Default)]
struct ServerState {
    infected: bool,
    root: PathBuf,
    seen: Mutex<Vec<Seen>>,
}

/// The recorded WordPress site, served on loopback.
pub struct FixtureServer {
    pub origin: String,
    state: Arc<ServerState>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FixtureServer {
    pub async fn start(infected: bool) -> Self {
        Self::start_at(fixtures(), infected).await
    }

    pub async fn start_at(root: PathBuf, infected: bool) -> Self {
        let state = Arc::new(ServerState { infected, root, seen: Mutex::new(Vec::new()) });
        let app = Router::new().fallback(answer).with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { origin, state, task }
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.state.seen.lock().unwrap().clone()
    }

    pub fn requests(&self) -> usize {
        self.state.seen.lock().unwrap().len()
    }

    pub fn url(&self) -> Url {
        Url::parse(&self.origin).unwrap()
    }
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" | "php" => "text/html; charset=UTF-8",
        "xml" => "application/xml",
        "json" => "application/json",
        "css" => "text/css",
        "js" => "application/javascript",
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

fn respond(status: u16, kind: &str, body: Vec<u8>) -> Response<Body> {
    Response::builder().status(status).header("content-type", kind).body(Body::from(body)).unwrap()
}

async fn answer(
    State(state): State<Arc<ServerState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response<Body> {
    let user_agent =
        headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    state.seen.lock().unwrap().push(Seen {
        path: uri.to_string(),
        user_agent: user_agent.clone(),
        signed: headers.contains_key("signature") && headers.contains_key("signature-input"),
    });
    if method != Method::GET {
        return respond(405, "text/plain", Vec::new());
    }
    let path = uri.path();
    let query = uri.query().unwrap_or_default();
    let root = state.root.clone();
    if let Some(route) = path.strip_prefix("/wp-json/") {
        let authorised = headers.get("authorization").and_then(|v| v.to_str().ok())
            == Some(&format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(APP_PASSWORD)
            ));
        let drafts = query.contains("status=");
        if (drafts || route == "wp/v2/menu-items") && !authorised {
            return respond(401, "application/json", br#"{"code":"rest_forbidden"}"#.to_vec());
        }
        if query.contains("page=2") {
            return respond(
                400,
                "application/json",
                br#"{"code":"rest_post_invalid_page_number"}"#.to_vec(),
            );
        }
        let file = match (route.trim_end_matches('/'), drafts) {
            ("", _) => "index.json",
            ("wp/v2/posts", true) => "posts-drafts.json",
            ("wp/v2/pages", true) => "",
            ("wp/v2/posts", false) => "posts.json",
            ("wp/v2/pages", false) => "pages.json",
            ("wp/v2/categories", _) => "categories.json",
            ("wp/v2/tags", _) => "tags.json",
            ("wp/v2/media", _) => "media.json",
            ("wp/v2/menu-items", _) => "menu-items.json",
            ("wc/store/v1/products", _) => "products.json",
            _ => return respond(404, "application/json", br#"{"code":"rest_no_route"}"#.to_vec()),
        };
        let body = if file.is_empty() {
            b"[]".to_vec()
        } else {
            std::fs::read(root.join("wordpress").join(file)).unwrap()
        };
        return Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .header("x-wp-totalpages", "1")
            .body(Body::from(body))
            .unwrap();
    }
    if path == "/old-offers/" {
        return Response::builder()
            .status(301)
            .header("location", format!("{ORIGIN}/products/panel-400/"))
            .body(Body::empty())
            .unwrap();
    }
    let crawler =
        user_agent.contains("adk-site") || user_agent.to_ascii_lowercase().contains("bot");
    if state.infected && path == "/" && crawler {
        let body = std::fs::read(root.join("infected/index.crawler.html")).unwrap();
        return respond(200, content_type("index.html"), body);
    }
    let relative = path.trim_start_matches('/');
    let relative = if relative.is_empty() || relative.ends_with('/') {
        format!("{relative}index.html")
    } else {
        relative.to_string()
    };
    if relative.split('/').any(|s| s == "..") {
        return respond(400, "text/plain", Vec::new());
    }
    let mut candidates = Vec::new();
    if state.infected {
        candidates.push(root.join("infected").join(&relative));
    }
    candidates.push(root.join("site").join(&relative));
    for candidate in candidates {
        if candidate.is_file() {
            return respond(200, content_type(&relative), std::fs::read(candidate).unwrap());
        }
    }
    respond(404, "text/html", b"<html><body>Not found</body></html>".to_vec())
}

/// The site's Web Bot Auth signer.
pub fn signer() -> Arc<dyn CrawlSigner> {
    let key = Arc::new(Ed25519SigningKey::generate().unwrap());
    Arc::new(HttpSignatureSigner::builder(key, ORIGIN).build().unwrap())
}

/// A client that reaches only `server`, with the public origin mapped onto it.
pub fn client(server: &FixtureServer) -> Arc<HttpClient> {
    Arc::new(
        HttpClient::builder()
            .egress(EgressScope::Only(BTreeSet::from([server.origin.clone()])))
            .map_origin(&Url::parse(ORIGIN).unwrap(), server.url())
            .min_interval(Duration::ZERO)
            .signer(signer())
            .build()
            .unwrap(),
    )
}

pub fn acting() -> ActingContext {
    ActingContext::derived(DOMAIN, "migrator", "owner", SubjectKind::Human)
}

/// A polite context over `server`.
pub fn context(server: &FixtureServer) -> SourceContext {
    SourceContext::new(acting(), client(server))
}

/// A proof of control of `domain`, from a DNS claim.
pub async fn proven(domain: &str) -> ProvenDomain {
    let claim = DomainClaim::new(domain, chrono::Utc::now()).unwrap();
    let (name, value) = claim.dns_record();
    let dns = StaticDnsResolver::new();
    dns.set(&name, RecordKind::Txt, [value]).await;
    claim.verify_dns(&dns, chrono::Utc::now()).await.unwrap()
}

/// A context over `server` on the proven fixture domain.
pub async fn proven_context(server: &FixtureServer) -> SourceContext {
    context(server).with_scope(ClaimScope::Proven(proven(DOMAIN).await))
}

/// A copy of the legacy uploads directory, with the infected overlay's files when asked.
pub fn uploads(infected: bool) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut sources = vec![fixtures().join("site/wp-content/uploads")];
    if infected {
        sources.push(fixtures().join("infected/wp-content/uploads"));
    }
    for source in sources {
        copy_tree(&source, dir.path());
    }
    dir
}

pub fn copy_tree(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Issues the fixture's application password with the given scopes.
#[derive(Debug)]
pub struct TestBroker {
    pub scopes: Vec<String>,
}

#[async_trait]
impl CredentialBroker for TestBroker {
    async fn token_for(
        &self,
        acting: &ActingContext,
        resource: &Url,
        _scopes: &[String],
    ) -> AdkResult<ScopedToken> {
        Ok(ScopedToken::new(
            acting.agent.id.clone(),
            APP_PASSWORD,
            resource.clone(),
            self.scopes.clone(),
        ))
    }

    async fn revoke(&self, _scope: &RevocationScope) -> AdkResult<u64> {
        Ok(0)
    }
}

/// Replays the recorded site from an in-process server in place of containers: the driver
/// contract without Docker.
#[derive(Debug, Default)]
pub struct FakeDriver {
    pub infected: bool,
    server: tokio::sync::Mutex<Option<FixtureServer>>,
    pub started: Mutex<Vec<ReplaySpec>>,
    pub destroyed: Mutex<Vec<String>>,
}

impl std::fmt::Debug for FixtureServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixtureServer").field("origin", &self.origin).finish()
    }
}

#[async_trait]
impl ContainerDriver for FakeDriver {
    async fn start(&self, spec: &ReplaySpec) -> adk_site::migrate::Result<ReplayContainer> {
        let server = FixtureServer::start(self.infected).await;
        let origin = server.origin.clone();
        *self.server.lock().await = Some(server);
        self.started.lock().unwrap().push(spec.clone());
        Ok(ReplayContainer {
            id: "fake".into(),
            origin,
            network: "fake-internal".into(),
            containers: vec!["fake-web".into()],
            egress_blocked: true,
        })
    }

    async fn egress_attempts(
        &self,
        _replay: &ReplayContainer,
    ) -> adk_site::migrate::Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn destroy(&self, replay: &ReplayContainer) -> adk_site::migrate::Result<()> {
        self.server.lock().await.take();
        self.destroyed.lock().unwrap().push(replay.id.clone());
        Ok(())
    }
}

impl FakeDriver {
    /// A driver replaying the recorded site, with the infected overlay when asked.
    pub fn new(infected: bool) -> Self {
        Self { infected, ..Self::default() }
    }

    /// Requests the replayed server saw, while it runs.
    pub async fn requests(&self) -> usize {
        self.server.lock().await.as_ref().map_or(0, FixtureServer::requests)
    }
}
