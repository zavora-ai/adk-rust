//! The approvals inbox a kernel's control plane serves, read in full (spec 3.3 §6, §10 C1).
//!
//! `ControlClient` reads the inbox as `ApprovalItem`s: pending only, without the risk, the
//! acting agent or the decisions, and decides without edited arguments. The engine needs the
//! whole request (spec 3.3 §7.1), so [`ControlSource`](crate::ControlSource) reads
//! `GET /api/approvals?session=`, `GET /api/approvals/{id}` and
//! `POST /api/approvals/{id}/decide` itself, over the same transport the client uses: HTTP,
//! or HTTP over the kernel's Unix socket. Requests come without their fingerprint, which the
//! inbox never serves.

use std::path::PathBuf;

use adk_core::{
    AdkError, ApprovalAction, ApprovalDecision, ApprovalRequest, ApprovalState, ErrorCategory,
    ErrorComponent,
};
use serde_json::{Value, json};

use crate::error::{Result, SurfaceError};

/// How many requests one page reads.
const PAGE: usize = 200;

/// The error codes of the approvals inbox the engine tells apart.
const CODES: &[&str] = &[
    "approval.version_conflict",
    "approval.already_resolved",
    "approval.already_decided",
    "approval.expired",
    "approval.not_found",
    "approval.not_an_approver",
    "approval.self_approval",
    "approval.delegation_inactive",
    "approval.action_not_allowed",
    "approval.invalid_state",
    "approval.invalid_cursor",
];

/// Where the inbox is served.
#[derive(Debug, Clone)]
pub(crate) enum Inbox {
    /// `base` + `/api/approvals`, with a bearer token when the control plane needs one.
    Http { base: String, client: reqwest::Client, token: Option<String> },
    /// HTTP over the kernel's Unix socket, as the OS user.
    #[cfg_attr(not(unix), allow(dead_code))]
    Socket(PathBuf),
}

impl Inbox {
    /// The inbox beside the control plane `endpoint` (`ControlClient::endpoint`), or `None`
    /// for a named pipe, whose clients keep to `ControlClient`.
    pub(crate) fn for_endpoint(endpoint: &str) -> Option<Self> {
        if let Some(path) = endpoint.strip_prefix("unix:") {
            return cfg!(unix).then(|| Self::Socket(PathBuf::from(path)));
        }
        if endpoint.starts_with("pipe:") {
            return None;
        }
        Some(Self::Http {
            base: endpoint.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
            token: None,
        })
    }

    pub(crate) fn with_token(self, token: String) -> Self {
        match self {
            Self::Http { base, client, .. } => Self::Http { base, client, token: Some(token) },
            socket => socket,
        }
    }

    /// The caller's requests — of `session` when set — open ones only when `open_only`.
    pub(crate) async fn list(
        &self,
        app: &str,
        session: Option<&str>,
        open_only: bool,
    ) -> Result<Vec<ApprovalRequest>> {
        let states: Vec<&str> = if open_only {
            vec![ApprovalState::Pending.as_str()]
        } else {
            ApprovalState::ALL.iter().map(|state| state.as_str()).collect()
        };
        let mut requests = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut query =
                format!("state={}&app={}&limit={PAGE}", encode(&states.join(",")), encode(app));
            if let Some(session) = session {
                query.push_str(&format!("&session={}", encode(session)));
            }
            if let Some(cursor) = &cursor {
                query.push_str(&format!("&cursor={}", encode(cursor)));
            }
            let page = self.call("GET", &format!("/api/approvals?{query}"), None).await?;
            for item in page.get("items").and_then(Value::as_array).into_iter().flatten() {
                requests.push(request_of(item.clone())?);
            }
            match page.get("nextCursor").and_then(Value::as_str) {
                Some(next) => cursor = Some(next.to_string()),
                None => break,
            }
        }
        Ok(requests)
    }

    /// One request, or `None` when the caller may not see it or it does not exist.
    pub(crate) async fn get(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        match self.call("GET", &format!("/api/approvals/{}", encode(id)), None).await {
            Ok(view) => request_of(view).map(Some),
            Err(SurfaceError::Source(error)) if error.category == ErrorCategory::NotFound => {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// `POST /api/approvals/{id}/decide` as the authenticated caller.
    pub(crate) async fn decide(
        &self,
        id: &str,
        expected_version: u64,
        decision: &ApprovalDecision,
    ) -> Result<ApprovalRequest> {
        let action = match decision.kind {
            ApprovalAction::Approve => "approve",
            ApprovalAction::Edit => "edit",
            ApprovalAction::Reject => "reject",
            ApprovalAction::Respond => "respond",
        };
        let body = json!({
            "expectedVersion": expected_version,
            "action": action,
            "editedArgs": decision.edited_args,
            "reason": decision.reason,
            "via": decision.via_delegation,
        });
        let view =
            self.call("POST", &format!("/api/approvals/{}/decide", encode(id)), Some(body)).await?;
        request_of(view)
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let (status, bytes) = match self {
            Self::Http { base, client, token } => {
                let method = reqwest::Method::from_bytes(method.as_bytes())
                    .map_err(|error| SurfaceError::Invalid(error.to_string()))?;
                let mut request = client.request(method, format!("{base}{path}"));
                if let Some(token) = token {
                    request = request.bearer_auth(token);
                }
                if let Some(body) = &body {
                    request = request.json(body);
                }
                let response = request.send().await.map_err(|error| unreachable(&error))?;
                let status = response.status().as_u16();
                (status, response.bytes().await.map_err(|error| unreachable(&error))?.to_vec())
            }
            Self::Socket(socket) => socket_call(socket, method, path, body).await?,
        };
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok(value);
        }
        Err(SurfaceError::Source(problem(status, &value)))
    }
}

/// One HTTP/1.1 exchange over the kernel's Unix socket.
#[cfg(unix)]
async fn socket_call(
    socket: &std::path::Path,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<(u16, Vec<u8>)> {
    use http_body_util::{BodyExt, Full};
    use hyper::body::Bytes;

    let stream =
        tokio::net::UnixStream::connect(socket).await.map_err(|error| unreachable(&error))?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .map_err(|error| unreachable(&error))?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::debug!(error = %error, "the kernel socket connection closed");
        }
    });
    let body = body.map(|body| body.to_string()).unwrap_or_default();
    let request = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost")
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body)))
        .map_err(|error| SurfaceError::Invalid(error.to_string()))?;
    let response = sender.send_request(request).await.map_err(|error| unreachable(&error))?;
    let status = response.status().as_u16();
    let bytes = response.into_body().collect().await.map_err(|error| unreachable(&error))?;
    Ok((status, bytes.to_bytes().to_vec()))
}

#[cfg(not(unix))]
async fn socket_call(
    _socket: &std::path::Path,
    _method: &str,
    _path: &str,
    _body: Option<Value>,
) -> Result<(u16, Vec<u8>)> {
    Err(SurfaceError::unsupported("the kernel's Unix socket on this platform"))
}

fn unreachable(error: &dyn std::fmt::Display) -> SurfaceError {
    SurfaceError::Source(AdkError::new(
        ErrorComponent::Server,
        ErrorCategory::Unavailable,
        "surface.kernel_unreachable",
        format!(
            "the kernel's approvals inbox could not be reached: {error}; is the kernel running?"
        ),
    ))
}

/// The problem document an inbox route answered, as an error with the inbox's own code when
/// the engine tells it apart (a version conflict shows who decided, S-I2).
fn problem(status: u16, value: &Value) -> AdkError {
    let error = value.get("error");
    let detail = error.and_then(Value::as_object);
    let code = detail
        .and_then(|detail| detail.get("code"))
        .and_then(Value::as_str)
        .and_then(|code| CODES.iter().copied().find(|known| *known == code))
        .unwrap_or("surface.source");
    let category = detail
        .and_then(|detail| detail.get("category"))
        .and_then(|category| serde_json::from_value(category.clone()).ok())
        .unwrap_or(match status {
            401 => ErrorCategory::Unauthorized,
            403 => ErrorCategory::Forbidden,
            404 => ErrorCategory::NotFound,
            409 => ErrorCategory::Conflict,
            400 | 422 => ErrorCategory::InvalidInput,
            _ => ErrorCategory::Unavailable,
        });
    let message = detail
        .and_then(|detail| detail.get("message"))
        .and_then(Value::as_str)
        .or_else(|| error.and_then(Value::as_str))
        .unwrap_or("the kernel's approvals inbox refused the request")
        .to_string();
    AdkError::new(ErrorComponent::Server, category, code, message)
}

/// A request as the inbox serves it, without its fingerprint.
fn request_of(mut view: Value) -> Result<ApprovalRequest> {
    if let Some(object) = view.as_object_mut() {
        object.entry("fingerprint").or_insert_with(|| Value::String(String::new()));
    }
    serde_json::from_value(view).map_err(|error| {
        SurfaceError::Invalid(format!(
            "the kernel answered a request the engine cannot read: {error}"
        ))
    })
}

/// Percent-encodes `text` for a path segment or a query value.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inbox_conflict_keeps_its_code() {
        let body = json!({ "error": { "code": "approval.version_conflict", "category": "conflict", "message": "re-read" } });
        let error = problem(409, &body);
        assert_eq!(
            (error.code, error.category),
            ("approval.version_conflict", ErrorCategory::Conflict)
        );
        let plain =
            problem(401, &json!({ "error": "approval endpoints need an authenticated caller" }));
        assert_eq!((plain.code, plain.category), ("surface.source", ErrorCategory::Unauthorized));
    }

    #[test]
    fn the_inbox_sits_beside_the_control_plane() {
        assert!(
            matches!(Inbox::for_endpoint("http://127.0.0.1:8080/"), Some(Inbox::Http { ref base, .. }) if base == "http://127.0.0.1:8080")
        );
        assert!(Inbox::for_endpoint(r"pipe:\\.\pipe\adk-rust-kernel-1").is_none());
        #[cfg(unix)]
        assert!(matches!(Inbox::for_endpoint("unix:/home/a/kernel.sock"), Some(Inbox::Socket(_))));
    }
}
