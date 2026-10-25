//! The web home's page (spec 3.3 §5.1, §10 S1): the React app built from `adk-surface/web`,
//! embedded in the binary and served at `/ui/`.

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use rust_embed::RustEmbed;

/// Where the page is served (spec 3.3 §5.1).
pub const HOME_PATH: &str = "/ui/";

/// The page may load only what it ships with and talk only to the server that served it.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; connect-src 'self'; \
     img-src 'self' data: blob:; style-src 'self'; script-src 'self'; font-src 'self'; \
     object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'";

/// The page's policy for a request to `headers`' host: [`CONTENT_SECURITY_POLICY`], with the
/// voice socket on that same host, `ws://{host} wss://{host}`, which not every browser counts
/// as `'self'` (spec 3.3 §5.6). The host is the one the server's host guard admitted; one
/// that is not a plain host and port gets no socket.
fn content_security_policy(headers: &HeaderMap) -> String {
    let host = headers.get(header::HOST).and_then(|host| host.to_str().ok()).filter(|host| {
        !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c))
    });
    match host {
        Some(host) => CONTENT_SECURITY_POLICY.replacen(
            "connect-src 'self'",
            &format!("connect-src 'self' ws://{host} wss://{host}"),
            1,
        ),
        None => CONTENT_SECURITY_POLICY.to_string(),
    }
}

#[derive(RustEmbed)]
#[folder = "assets/web"]
struct Assets;

/// The web home's page as root routes (spec 3.3 §5.1, §10 S1): `/` redirects to `/ui/`,
/// which serves the app; its sections (`/ui/today`, `/ui/memory`, `/ui/developer`) and every
/// decision's deep link (`/ui/decisions/<id>`, §7.2) serve the same page. Mount it with
/// `ServerBuilder::add_root_routes`, beside [`router`](super::router) under `/api`, which is
/// what the page reads and acts through.
///
/// The page itself is public, like any static asset; every route it calls needs the caller
/// the server's auth middleware admits (S-I8). `index.html` carries a strict content
/// security policy: scripts, styles and fonts from the server only, no framing.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::{LocalSource, SurfaceEngine};
///
/// # fn example(source: LocalSource, builder: adk_server::ServerBuilder) {
/// let engine = SurfaceEngine::builder(Arc::new(source)).build();
/// let builder = builder
///     .add_api_routes(adk_surface::web::router(engine))
///     .add_root_routes(adk_surface::web::home());
/// # let _ = builder;
/// # }
/// ```
pub fn home() -> Router {
    Router::new()
        .route("/", get(|| async { Redirect::to(HOME_PATH) }))
        .route("/ui", get(|| async { Redirect::to(HOME_PATH) }))
        .route(HOME_PATH, get(index))
        .route("/ui/{*path}", get(asset))
}

async fn index(headers: HeaderMap) -> Response {
    embedded("index.html", &headers)
}

async fn asset(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches(HOME_PATH);
    if Assets::get(path).is_some() {
        return embedded(path, &headers);
    }
    // A section of the app (`/ui/today`) is the page; a missing file is not. A decision's deep
    // link (spec 3.3 §7.2) is always the page: its id may contain a dot.
    if !path.starts_with("decisions/")
        && path.rsplit('/').next().is_some_and(|name| name.contains('.'))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    embedded("index.html", &headers)
}

fn embedded(path: &str, headers: &HeaderMap) -> Response {
    let Some(file) = Assets::get(path) else {
        return (
            StatusCode::NOT_FOUND,
            "the web home's assets are missing from this build: run `npm run verify` in \
             adk-surface/web",
        )
            .into_response();
    };
    let page = path == "index.html";
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let policy = page.then(|| content_security_policy(headers));
    let mut response = Body::from(file.data).into_response();
    let headers = response.headers_mut();
    if let Ok(value) = header::HeaderValue::from_str(mime.as_ref()) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    // Built assets carry a content hash in their names; the page that names them does not,
    // nor do the phone's service worker, manifest and icons, whose names browsers fix
    // (spec 3.3 §5.4).
    let fixed_name =
        path == "sw.js" || path == "manifest.webmanifest" || path.starts_with("icons/");
    headers.insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static(if page || fixed_name {
            "no-cache"
        } else {
            "public, max-age=31536000, immutable"
        }),
    );
    headers.insert(header::X_CONTENT_TYPE_OPTIONS, header::HeaderValue::from_static("nosniff"));
    headers.insert(header::REFERRER_POLICY, header::HeaderValue::from_static("no-referrer"));
    if let Some(value) = policy.and_then(|policy| header::HeaderValue::from_str(&policy).ok()) {
        headers.insert(header::CONTENT_SECURITY_POLICY, value);
    }
    response
}
