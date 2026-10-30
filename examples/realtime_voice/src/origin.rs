//! Same-origin guard for every route the server answers.
//!
//! Browsers apply no CORS to WebSocket handshakes, and they send a simple cross-site `POST`
//! without a preflight, so CORS alone lets any site the user visits open `/ws` or change
//! server state. This guard refuses each request whose `Origin` is not the page the server
//! serves.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tracing::warn;

/// Answers `403 Forbidden` to a request whose `Origin` is not this server's page.
///
/// Install it with `axum::middleware::from_fn_with_state(bound, require_own_origin)`, where
/// `bound` is the listener's local address.
pub async fn require_own_origin(
    State(bound): State<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    let origin = headers.get(header::ORIGIN);
    if origin_allowed(origin, headers.get(header::HOST), bound) {
        return next.run(request).await;
    }
    warn!(?origin, path = request.uri().path(), "refused a request from another origin");
    (StatusCode::FORBIDDEN, "requests are accepted only from this server's page").into_response()
}

/// Whether a request comes from the page this server serves.
///
/// The page's origin is `http://` and the bound address, where any loopback address and
/// `localhost` count as one host. On an unspecified bind (`0.0.0.0`, `::`) the page's address
/// is the one the browser connected to, which it names in `Host`. Host names other than
/// `localhost` are refused, which also stops DNS rebinding. A request without `Origin` is let
/// through: browsers send it on every WebSocket handshake and cross-site `POST`, and a
/// non-browser client could send any `Origin` it liked, so the bind address is what limits it.
pub(crate) fn origin_allowed(
    origin: Option<&HeaderValue>,
    host: Option<&HeaderValue>,
    bound: SocketAddr,
) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let Some((origin_ip, origin_port)) = origin
        .to_str()
        .ok()
        .and_then(|origin| origin.strip_prefix("http://"))
        .and_then(ip_authority)
    else {
        return false;
    };
    let same_host = |ip: IpAddr| ip == origin_ip || (ip.is_loopback() && origin_ip.is_loopback());
    if origin_port != bound.port() {
        return false;
    }
    if bound.ip().is_unspecified() {
        return host
            .and_then(|host| host.to_str().ok())
            .and_then(ip_authority)
            .is_some_and(|(ip, port)| port == origin_port && same_host(ip));
    }
    same_host(bound.ip())
}

/// Parse `host[:port]` into an IP address and a port (default 80), reading `localhost` as
/// `127.0.0.1`. Any other host name yields `None`.
fn ip_authority(authority: &str) -> Option<(IpAddr, u16)> {
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.ends_with(']') => (host, port.parse().ok()?),
        _ => (authority, 80),
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let ip = if host.eq_ignore_ascii_case("localhost") {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        host.parse().ok()?
    };
    Some((ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_rules() {
        let loopback: SocketAddr = "127.0.0.1:3000".parse().unwrap();
        let lan: SocketAddr = "192.168.1.10:3000".parse().unwrap();
        let every: SocketAddr = "0.0.0.0:3000".parse().unwrap();
        let cases = [
            // (origin, host, bound, allowed)
            (None, Some("127.0.0.1:3000"), loopback, true),
            (Some("http://127.0.0.1:3000"), Some("127.0.0.1:3000"), loopback, true),
            (Some("http://localhost:3000"), Some("localhost:3000"), loopback, true),
            (Some("http://[::1]:3000"), Some("[::1]:3000"), loopback, true),
            (Some("http://evil.example"), Some("127.0.0.1:3000"), loopback, false),
            (Some("http://localhost:8080"), Some("127.0.0.1:3000"), loopback, false),
            (Some("https://localhost:3000"), Some("127.0.0.1:3000"), loopback, false),
            (Some("null"), Some("127.0.0.1:3000"), loopback, false),
            (
                Some("http://rebind.evil.example:3000"),
                Some("rebind.evil.example:3000"),
                loopback,
                false,
            ),
            (Some("http://192.168.1.10:3000"), Some("192.168.1.10:3000"), lan, true),
            (Some("http://localhost:3000"), Some("localhost:3000"), lan, false),
            (Some("http://192.168.1.10:3000"), Some("192.168.1.10:3000"), every, true),
            (Some("http://localhost:3000"), Some("127.0.0.1:3000"), every, true),
            (Some("http://203.0.113.5:3000"), Some("192.168.1.10:3000"), every, false),
            (Some("http://lan-host:3000"), Some("lan-host:3000"), every, false),
            (Some("http://192.168.1.10:3000"), None, every, false),
        ];
        for (origin, host, bound, allowed) in cases {
            let origin = origin.map(HeaderValue::from_static);
            let host = host.map(HeaderValue::from_static);
            assert_eq!(
                origin_allowed(origin.as_ref(), host.as_ref(), bound),
                allowed,
                "origin {origin:?}, host {host:?}, bound {bound}"
            );
        }
    }
}
