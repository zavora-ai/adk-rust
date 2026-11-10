//! A2A v1.0.0 version negotiation per spec §3.6.
//!
//! Provides an Axum middleware that reads the requested protocol version from
//! the `A2A-Version` header (or an `A2A-Version` query parameter, which §3.6.1
//! also allows), validates it against [`SUPPORTED_VERSIONS`], and either sets
//! the negotiated version on the response or returns a
//! [`VersionNotSupportedError`](super::error::A2aError::VersionNotSupported)
//! in the shape of the binding the request used.

use std::collections::HashMap;

use super::error::A2aError;
use super::jsonrpc_handler;
use axum::{
    extract::Query,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};

/// Protocol versions the A2A v1 handlers serve.
///
/// `0.3` is not one of them. The v1 handlers answer with v1 payloads only, and
/// §3.6.2 says a request without `A2A-Version` means 0.3, so such a request
/// gets [`VersionNotSupported`](A2aError::VersionNotSupported) rather than v1
/// payloads a 0.3 client cannot read. Serve 0.3 clients from the 0.3
/// controller ([`ServerBuilder::with_a2a`](crate::ServerBuilder::with_a2a)).
pub const SUPPORTED_VERSIONS: &[&str] = &["1.0"];

/// The version a request without `A2A-Version`, or with an empty one, asks for
/// (§3.6.2: "Agents MUST interpret empty value as 0.3 version").
const UNVERSIONED: &str = "0.3";

/// Header name for A2A protocol version negotiation.
const A2A_VERSION_HEADER: &str = "a2a-version";

/// The most of a request body read to answer an unsupported version on the
/// JSON-RPC binding; axum's default body limit, which the handlers apply too.
const MAX_ERROR_BODY: usize = 2 * 1024 * 1024;

/// Pure function for version negotiation logic (testable without Axum).
///
/// Returns the negotiated version string on success, or a
/// [`VersionNotSupported`](A2aError::VersionNotSupported) error when the
/// requested version is not in [`SUPPORTED_VERSIONS`].
///
/// # Rules
///
/// - `None` or an empty string means `"0.3"` (§3.6.2), which the v1 handlers
///   do not serve, so it is an error.
/// - Versions compare by `Major.Minor`; a patch number is ignored (§3.6), so
///   `"1.0.1"` negotiates `"1.0"`.
/// - Anything else is an error listing the supported versions.
///
/// # Example
///
/// ```
/// use adk_server::a2a::v1::version::negotiate_version;
///
/// assert_eq!(negotiate_version(Some("1.0")).unwrap(), "1.0");
/// assert_eq!(negotiate_version(Some("1.0.1")).unwrap(), "1.0");
/// assert!(negotiate_version(Some("99.0")).is_err());
/// assert!(negotiate_version(None).is_err(), "no header means 0.3");
/// ```
pub fn negotiate_version(requested: Option<&str>) -> Result<&'static str, A2aError> {
    let requested = requested.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(UNVERSIONED);
    major_minor(requested)
        .and_then(|wanted| {
            SUPPORTED_VERSIONS.iter().copied().find(|&s| major_minor(s) == Some(wanted))
        })
        .ok_or_else(|| A2aError::VersionNotSupported {
            requested: requested.to_string(),
            supported: SUPPORTED_VERSIONS.iter().map(|s| (*s).to_string()).collect(),
        })
}

/// The `(major, minor)` of a `Major.Minor[.Patch]` version, or `None` when
/// `version` has another form.
fn major_minor(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    if let Some(patch) = parts.next() {
        patch.parse::<u32>().ok()?;
    }
    parts.next().is_none().then_some((major, minor))
}

/// The version `req` asks for: its `A2A-Version` header, else its
/// `A2A-Version` query parameter (matched without regard to case), else
/// `None`. A header value that is not visible ASCII is returned as `"?"`, so
/// it is refused rather than read as 0.3.
fn requested_version(req: &axum::extract::Request) -> Option<String> {
    if let Some(value) = req.headers().get(A2A_VERSION_HEADER) {
        return Some(value.to_str().unwrap_or("?").to_string());
    }
    let Query(params) = Query::<HashMap<String, String>>::try_from_uri(req.uri()).ok()?;
    params.into_iter().find(|(key, _)| key.eq_ignore_ascii_case(A2A_VERSION_HEADER)).map(|(_, v)| v)
}

/// Axum middleware for A2A version negotiation.
///
/// Reads the requested version (see the [module docs](self)), validates it
/// via [`negotiate_version`], and either:
/// - sets the `A2A-Version` response header to the negotiated version and
///   forwards the request, or
/// - answers with a `VersionNotSupportedError` listing the supported versions,
///   without calling the handler. A JSON-RPC request (a JSON object body with
///   a `jsonrpc` member) gets JSON-RPC error `-32009` with its `id`, as HTTP
///   200 `application/json` (§5.4, §9.5); any other request gets the HTTP+JSON
///   binding's HTTP 400 with an AIP-193 body (§11.6).
///
/// Mount it over the JSON-RPC and HTTP+JSON routes, not the agent card route:
/// card discovery carries no version.
pub async fn version_negotiation(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let requested = requested_version(&req);

    match negotiate_version(requested.as_deref()) {
        Ok(version) => {
            let mut response = next.run(req).await;
            if let Ok(value) = HeaderValue::from_str(version) {
                response.headers_mut().insert(A2A_VERSION_HEADER, value);
            }
            response
        }
        Err(err) => version_error(req, &err).await,
    }
}

/// Answers `req` with `err` in the shape of the binding `req` used.
async fn version_error(req: axum::extract::Request, err: &A2aError) -> Response {
    let body = axum::body::to_bytes(req.into_body(), MAX_ERROR_BODY).await.unwrap_or_default();
    if let Some(id) = jsonrpc_handler::jsonrpc_request_id(&body) {
        return jsonrpc_handler::error_response(id, err);
    }
    (
        StatusCode::from_u16(err.http_status()).unwrap_or(StatusCode::BAD_REQUEST),
        [(axum::http::header::CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        axum::Json(err.to_http_error_response()),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::Router;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::{get, post};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[test]
    fn supported_version_1_0_returns_ok() {
        assert_eq!(negotiate_version(Some("1.0")).unwrap(), "1.0");
    }

    #[test]
    fn patch_numbers_do_not_take_part_in_negotiation() {
        assert_eq!(negotiate_version(Some("1.0.0")).unwrap(), "1.0");
        assert_eq!(negotiate_version(Some("1.0.1")).unwrap(), "1.0");
        assert_eq!(negotiate_version(Some(" 1.0 ")).unwrap(), "1.0");
    }

    #[test]
    fn missing_or_empty_header_means_0_3_which_v1_does_not_serve() {
        for requested in [None, Some(""), Some("  ")] {
            match negotiate_version(requested).unwrap_err() {
                A2aError::VersionNotSupported { requested, supported } => {
                    assert_eq!(requested, "0.3");
                    assert_eq!(supported, ["1.0"]);
                }
                other => panic!("expected VersionNotSupported, got: {other}"),
            }
        }
    }

    #[test]
    fn explicit_0_3_is_not_served_by_the_v1_handlers() {
        assert!(negotiate_version(Some("0.3")).is_err());
    }

    #[test]
    fn unsupported_version_returns_error_with_supported_list() {
        let result = negotiate_version(Some("2.0"));
        let err = result.unwrap_err();
        match &err {
            A2aError::VersionNotSupported { requested, supported } => {
                assert_eq!(requested, "2.0");
                assert_eq!(supported, &["1.0"]);
            }
            other => panic!("expected VersionNotSupported, got: {other}"),
        }
        assert_eq!(err.json_rpc_code(), -32009);
        assert_eq!(err.http_status(), 400);
    }

    #[test]
    fn unsupported_version_0_1_returns_error() {
        let result = negotiate_version(Some("0.1"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        match &err {
            A2aError::VersionNotSupported { requested, .. } => {
                assert_eq!(requested, "0.1");
            }
            other => panic!("expected VersionNotSupported, got: {other}"),
        }
    }

    #[test]
    fn malformed_versions_return_error() {
        for v in ["not-a-version", "1", "1.", ".0", "1.0.x", "1.0.0.0", "v1.0"] {
            assert!(negotiate_version(Some(v)).is_err(), "{v}");
        }
    }

    #[test]
    fn all_supported_versions_return_ok() {
        for &version in SUPPORTED_VERSIONS {
            let result = negotiate_version(Some(version));
            assert_eq!(result.unwrap(), version);
        }
    }

    /// A router with a JSON-RPC-style POST route and a REST-style GET route
    /// behind the middleware, both answering `ok`.
    fn app() -> Router {
        Router::new()
            .route("/", post(|| async { "ok" }))
            .route("/tasks/{id}", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn(version_negotiation))
    }

    async fn send(request: Request<Body>) -> (axum::http::response::Parts, Vec<u8>) {
        let (parts, body) = app().oneshot(request).await.unwrap().into_parts();
        (parts, body.collect().await.unwrap().to_bytes().to_vec())
    }

    fn jsonrpc(version: Option<&str>) -> Request<Body> {
        let mut builder = Request::post("/").header("content-type", "application/json");
        if let Some(version) = version {
            builder = builder.header("A2A-Version", version);
        }
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": "req-7", "method": "SendMessage",
            "params": { "message": { "messageId": "m", "role": "ROLE_USER", "parts": [{"text": "hi"}] } }
        });
        builder.body(Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn a_supported_version_reaches_the_handler_and_is_echoed() {
        let (parts, body) = send(jsonrpc(Some("1.0"))).await;
        assert_eq!(parts.status, StatusCode::OK);
        assert_eq!(parts.headers[A2A_VERSION_HEADER], "1.0");
        assert_eq!(body, b"ok");
    }

    #[tokio::test]
    async fn an_unsupported_version_on_json_rpc_is_a_json_rpc_error() {
        for version in [Some("99.0"), None] {
            let (parts, body) = send(jsonrpc(version)).await;
            assert_eq!(parts.status, StatusCode::OK, "{version:?}");
            assert_eq!(parts.headers["content-type"], "application/json");
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["jsonrpc"], "2.0");
            assert_eq!(body["id"], "req-7", "the error answers the request's id");
            assert_eq!(body["error"]["code"], -32009);
            assert_eq!(body["error"]["data"][0]["reason"], "VERSION_NOT_SUPPORTED");
            assert_eq!(body["error"]["data"][0]["metadata"]["supported"], "1.0");
        }
    }

    #[tokio::test]
    async fn an_unsupported_version_on_rest_is_an_aip_193_error() {
        let request = Request::get("/tasks/t-1").header("A2A-Version", "99.0");
        let (parts, body) = send(request.body(Body::empty()).unwrap()).await;
        assert_eq!(parts.status, StatusCode::BAD_REQUEST);
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], 400);
        assert_eq!(body["error"]["status"], "FAILED_PRECONDITION");
        assert!(body.get("jsonrpc").is_none());
    }

    #[tokio::test]
    async fn the_version_may_be_a_query_parameter() {
        let request = Request::get("/tasks/t-1?A2A-Version=1.0").body(Body::empty()).unwrap();
        let (parts, body) = send(request).await;
        assert_eq!(parts.status, StatusCode::OK);
        assert_eq!(body, b"ok");

        let request = Request::get("/tasks/t-1?a2a-version=99.0").body(Body::empty()).unwrap();
        assert_eq!(send(request).await.0.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn the_header_wins_over_the_query_parameter() {
        let request = Request::get("/tasks/t-1?A2A-Version=1.0")
            .header("A2A-Version", "99.0")
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(request).await.0.status, StatusCode::BAD_REQUEST);
    }
}
