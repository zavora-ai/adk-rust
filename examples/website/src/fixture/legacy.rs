//! The legacy sites the scenarios start from (spec 3.5 §11 "Fixtures"), served on loopback:
//! the recorded WordPress site with its contact form and small shop, its `infected` overlay of
//! inert markers, and the business-presence pages. The compose fixture under
//! `fixtures/wordpress/` serves the same site from a real, patched WordPress in the nightly
//! tier; these recordings are what the pull-request tier runs against.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_auth::web_bot_auth::{Ed25519SigningKey, HttpSignatureSigner};
use adk_core::{ActingContext, SubjectKind};
use adk_site::migrate::claim::{DomainClaim, RecordKind, StaticDnsResolver};
use adk_site::migrate::http::{CrawlSigner, EgressScope, HttpClient};
use adk_site::migrate::sources::{ContainerDriver, ReplayContainer, ReplaySpec};
use adk_site::migrate::{ClaimScope, ProvenDomain, SourceContext};
use anyhow::Result;
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
/// The application password the broker issues; the recorded site accepts only it.
pub const APP_PASSWORD: &str = "owner:abcd efgh ijkl mnop";

/// The example's own fixtures.
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// The recorded WordPress site: REST collections, crawled pages, uploads and the `infected`
/// overlay, shared with `adk-site`'s migration tests so both run against one recording.
pub fn recorded_wordpress() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../adk-site/tests/fixtures/migrate")
}

/// One request the legacy server answered.
#[derive(Debug, Clone)]
pub struct Seen {
    /// Whether it carried an HTTP message signature.
    pub signed: bool,
}

#[derive(Debug, Default)]
struct ServerState {
    infected: bool,
    root: PathBuf,
    seen: Mutex<Vec<Seen>>,
}

/// A legacy site on loopback.
pub struct LegacyServer {
    /// Its loopback origin.
    pub origin: String,
    state: Arc<ServerState>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for LegacyServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl std::fmt::Debug for LegacyServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LegacyServer").field("origin", &self.origin).finish()
    }
}

impl LegacyServer {
    /// The recorded WordPress site, with the infected overlay when asked.
    ///
    /// # Errors
    ///
    /// When no loopback port can be bound.
    pub async fn wordpress(infected: bool) -> Result<Self> {
        Self::start_at(recorded_wordpress(), infected).await
    }

    /// The business-presence pages.
    ///
    /// # Errors
    ///
    /// When no loopback port can be bound.
    pub async fn presence() -> Result<Self> {
        let dir = fixtures().join("presence");
        Self::start_at(dir.parent().map(Path::to_path_buf).unwrap_or(dir), false).await
    }

    /// A server of `root`: `root/site` (and `root/infected` first when infected), the
    /// WordPress REST recordings under `root/wordpress`, the other fixture directories as
    /// they are.
    ///
    /// # Errors
    ///
    /// When no loopback port can be bound.
    pub async fn start_at(root: PathBuf, infected: bool) -> Result<Self> {
        let state = Arc::new(ServerState { infected, root, seen: Mutex::new(Vec::new()) });
        let app = Router::new().fallback(answer).with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                eprintln!("the legacy server stopped: {error}");
            }
        });
        Ok(Self { origin, state, task })
    }

    /// Every request it answered.
    pub fn seen(&self) -> Vec<Seen> {
        self.state.seen.lock().unwrap().clone()
    }

    /// How many requests it answered.
    pub fn requests(&self) -> usize {
        self.state.seen.lock().unwrap().len()
    }

    /// Its loopback origin as a URL.
    pub fn url(&self) -> Url {
        Url::parse(&self.origin).expect("a loopback origin")
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
    Response::builder()
        .status(status)
        .header("content-type", kind)
        .body(Body::from(body))
        .expect("a response")
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
            std::fs::read(root.join("wordpress").join(file)).unwrap_or_default()
        };
        return Response::builder()
            .status(200)
            .header("content-type", "application/json")
            .header("x-wp-totalpages", "1")
            .body(Body::from(body))
            .expect("a response");
    }
    if path == "/old-offers/" {
        return Response::builder()
            .status(301)
            .header("location", format!("{ORIGIN}/products/panel-400/"))
            .body(Body::empty())
            .expect("a response");
    }
    let crawler =
        user_agent.contains("adk-site") || user_agent.to_ascii_lowercase().contains("bot");
    if state.infected && path == "/" && crawler {
        let body = std::fs::read(root.join("infected/index.crawler.html")).unwrap_or_default();
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
    candidates.push(root.join(&relative));
    for candidate in candidates {
        if candidate.is_file() {
            return respond(
                200,
                content_type(&relative),
                std::fs::read(candidate).unwrap_or_default(),
            );
        }
    }
    respond(404, "text/html", b"<html><body>Not found</body></html>".to_vec())
}

/// The site's Web Bot Auth signer.
///
/// # Errors
///
/// When no key can be made.
pub fn signer() -> Result<Arc<dyn CrawlSigner>> {
    let key = Arc::new(Ed25519SigningKey::generate()?);
    Ok(Arc::new(HttpSignatureSigner::builder(key, ORIGIN).build()?))
}

/// A client that reaches only `server`, with the public origin mapped onto it.
///
/// # Errors
///
/// When the client cannot be built.
pub fn client(server: &LegacyServer) -> Result<Arc<HttpClient>> {
    Ok(Arc::new(
        HttpClient::builder()
            .egress(EgressScope::Only(BTreeSet::from([server.origin.clone()])))
            .map_origin(&Url::parse(ORIGIN)?, server.url())
            .min_interval(Duration::ZERO)
            .signer(signer()?)
            .build()?,
    ))
}

/// The migrator acting for the owner.
pub fn acting() -> ActingContext {
    ActingContext::derived(DOMAIN, "migrator", "owner", SubjectKind::Human)
}

/// A polite context over `server`.
///
/// # Errors
///
/// When the client cannot be built.
pub fn context(server: &LegacyServer) -> Result<SourceContext> {
    Ok(SourceContext::new(acting(), client(server)?))
}

/// A proof of control of `domain`, from a DNS claim.
///
/// # Errors
///
/// When the claim does not verify.
pub async fn proven(domain: &str) -> Result<ProvenDomain> {
    let claim = DomainClaim::new(domain, chrono::Utc::now())?;
    let (name, value) = claim.dns_record();
    let dns = StaticDnsResolver::new();
    dns.set(&name, RecordKind::Txt, [value]).await;
    Ok(claim.verify_dns(&dns, chrono::Utc::now()).await?)
}

/// A context over `server` on the proven legacy domain.
///
/// # Errors
///
/// When the client cannot be built or the claim does not verify.
pub async fn proven_context(server: &LegacyServer) -> Result<SourceContext> {
    Ok(context(server)?.with_scope(ClaimScope::Proven(proven(DOMAIN).await?)))
}

/// A copy of the legacy uploads directory, with the infected overlay's files when asked.
///
/// # Errors
///
/// When the copy fails.
pub fn uploads(infected: bool) -> Result<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    let mut sources = vec![recorded_wordpress().join("site/wp-content/uploads")];
    if infected {
        sources.push(recorded_wordpress().join("infected/wp-content/uploads"));
    }
    for source in sources {
        copy_tree(&source, dir.path())?;
    }
    Ok(dir)
}

/// Copies `from` into `to`.
///
/// # Errors
///
/// When a file cannot be read or written.
pub fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Replays the recorded site from an in-process server in place of containers: the
/// `ContainerDriver` contract without Docker. The nightly tier replays the compose fixture
/// through `DockerCliDriver` instead.
#[derive(Debug, Default)]
pub struct RecordedReplayDriver {
    infected: bool,
    server: tokio::sync::Mutex<Option<LegacyServer>>,
    /// The replays started.
    pub started: Mutex<Vec<ReplaySpec>>,
    /// The replays destroyed.
    pub destroyed: Mutex<Vec<String>>,
}

impl RecordedReplayDriver {
    /// A driver replaying the recorded site, with the infected overlay when asked.
    pub fn new(infected: bool) -> Self {
        Self { infected, ..Self::default() }
    }
}

#[async_trait::async_trait]
impl ContainerDriver for RecordedReplayDriver {
    async fn start(&self, spec: &ReplaySpec) -> adk_site::migrate::Result<ReplayContainer> {
        let server = LegacyServer::wordpress(self.infected)
            .await
            .map_err(|e| adk_site::migrate::MigrateError::Container(e.to_string()))?;
        let origin = server.origin.clone();
        *self.server.lock().await = Some(server);
        self.started.lock().unwrap().push(spec.clone());
        Ok(ReplayContainer {
            id: "recorded".into(),
            origin,
            network: "recorded-internal".into(),
            containers: vec!["recorded-web".into()],
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
