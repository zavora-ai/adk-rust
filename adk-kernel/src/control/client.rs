//! The control-plane client the CLI and other surfaces use (spec 3.2 §7.11, §8.1).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use adk_core::{AdkError, ApprovalAction, ErrorCategory, ErrorComponent};
use futures::stream::{self, BoxStream, StreamExt};
use hyper::body::Bytes;
use hyper::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::api::{
    ApprovalItem, AttachRequest, AttachResponse, FreezeRequest, FreezeResponse, KillRequest,
    NiceRequest, ProcessResponse, ProcessesQuery, ProcessesResponse, SpawnRequest,
};
use super::{ControlPlaneConfig, paths};
use crate::bus::EventRecord;
use crate::error::{self, KernelError, Result};
use crate::kernel::KernelHealth;
use crate::process::Process;
use crate::sched::QueueClass;

/// The approvals inbox, beside the kernel routes under `/api`.
const APPROVALS: &str = "/approvals";

/// How long a call other than [`events`](ControlClient::events) waits for the kernel.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// A client of a running kernel's control plane (spec 3.2 §7.11), over its Unix socket or
/// HTTP. Surfaces hold no state (K-I17): every call reads the kernel.
///
/// # Example
///
/// ```rust,no_run
/// use adk_kernel::control::ControlClient;
///
/// # async fn example(home: &std::path::Path) -> adk_kernel::Result<()> {
/// let client = match ControlClient::connect_local(home).await? {
///     Some(client) => client,
///     None => ControlClient::http("https://kernel.example.com").with_bearer_token("token"),
/// };
/// for process in client.processes(&Default::default()).await? {
///     println!("{} {:?}", process.meta.pid, process.run.state);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct ControlClient {
    transport: Transport,
    token: Option<String>,
}

#[derive(Debug, Clone)]
enum Transport {
    Socket(PathBuf),
    Http { base: String, client: reqwest::Client },
}

/// A response's status and body.
struct Answer {
    status: StatusCode,
    body: BoxStream<'static, Result<Bytes>>,
}

impl Answer {
    async fn bytes(self) -> Result<Vec<u8>> {
        let mut body = self.body;
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next().await {
            bytes.extend_from_slice(&chunk?);
        }
        Ok(bytes)
    }
}

impl ControlClient {
    /// A client of the kernel listening on the Unix socket at `path`.
    pub fn socket(path: impl Into<PathBuf>) -> Self {
        Self { transport: Transport::Socket(path.into()), token: None }
    }

    /// A client of the kernel serving HTTP at `base_url`, the routes under its `/api`.
    pub fn http(base_url: impl Into<String>) -> Self {
        let base = base_url.into().trim_end_matches('/').to_string();
        Self { transport: Transport::Http { base, client: reqwest::Client::new() }, token: None }
    }

    /// Sends `token` as a bearer token.
    #[must_use]
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Where the client connects: `unix:<path>` or the base URL.
    pub fn endpoint(&self) -> String {
        match &self.transport {
            Transport::Socket(path) => format!("unix:{}", path.display()),
            Transport::Http { base, .. } => base.clone(),
        }
    }

    /// A client of the kernel running at `home`, or `None` when none answers on its socket.
    ///
    /// A socket left by a kernel that did not stop cleanly counts as none. Off Unix there is
    /// no local socket (the named pipe is spec 3.2 §10 W2), so this is always `None`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the socket exists but cannot be read.
    pub async fn connect_local(home: &Path) -> Result<Option<Self>> {
        let Some(path) = ControlPlaneConfig::default().socket_path(home) else {
            return Ok(None);
        };
        connect_local_socket(path).await
    }

    /// `GET /kernel/health`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn health(&self) -> Result<KernelHealth> {
        self.call(Method::GET, paths::HEALTH.to_string(), None::<&()>).await
    }

    /// `GET /kernel/processes`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn processes(&self, query: &ProcessesQuery) -> Result<Vec<Process>> {
        let mut pairs = Vec::new();
        if let Some(partition) = &query.partition {
            pairs.push(format!("partition={}", encode(partition.as_str())));
        }
        if let Some(state) = query.state {
            let state = serde_json::to_value(state)?;
            pairs.push(format!("state={}", encode(state.as_str().unwrap_or_default())));
        }
        if let Some(limit) = query.limit {
            pairs.push(format!("limit={limit}"));
        }
        let path = match pairs.is_empty() {
            true => paths::PROCESSES.to_string(),
            false => format!("{}?{}", paths::PROCESSES, pairs.join("&")),
        };
        let answer: ProcessesResponse = self.call(Method::GET, path, None::<&()>).await?;
        Ok(answer.processes)
    }

    /// `POST /kernel/processes`: starts a process of a registered target as the caller.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `404` for an unknown target, `403` without the `operator`
    /// scope.
    pub async fn spawn(&self, request: &SpawnRequest) -> Result<Process> {
        let answer: ProcessResponse =
            self.call(Method::POST, paths::PROCESSES.to_string(), Some(request)).await?;
        Ok(answer.process)
    }

    /// `GET /approvals`: the held calls the caller may decide, pending first page.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `404` when it serves no approvals inbox.
    pub async fn approvals(&self) -> Result<Vec<ApprovalItem>> {
        #[derive(serde::Deserialize)]
        struct Page {
            items: Vec<ApprovalItem>,
        }
        let page: Page =
            self.call(Method::GET, format!("{APPROVALS}?limit=200"), None::<&()>).await?;
        Ok(page.items)
    }

    /// `POST /approvals/{id}/decide`: decides a held call, at the `version` the caller read.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered: `409` for a stale version, `403` for a caller who may not
    /// decide it.
    pub async fn decide(
        &self,
        id: &str,
        version: u64,
        action: ApprovalAction,
        reason: Option<&str>,
    ) -> Result<ApprovalItem> {
        let body = serde_json::json!({
            "expectedVersion": version,
            "action": action,
            "reason": reason,
        });
        let path = format!("{APPROVALS}/{}/decide", encode(id));
        self.call(Method::POST, path, Some(&body)).await
    }

    /// `POST /kernel/processes/{pid}/kill`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn kill(&self, pid: &str, reason: Option<&str>) -> Result<Process> {
        let body = KillRequest { reason: reason.map(str::to_string) };
        self.process_action(paths::KILL, pid, &body).await
    }

    /// `POST /kernel/processes/{pid}/nice`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn nice(&self, pid: &str, queue: QueueClass) -> Result<Process> {
        self.process_action(paths::NICE, pid, &NiceRequest { queue }).await
    }

    /// `POST /kernel/processes/{pid}/suspend`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn suspend(&self, pid: &str) -> Result<Process> {
        self.process_action(paths::SUSPEND, pid, &()).await
    }

    /// `POST /kernel/processes/{pid}/resume`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn resume(&self, pid: &str) -> Result<Process> {
        self.process_action(paths::RESUME, pid, &()).await
    }

    /// `POST /kernel/freeze`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn freeze(&self, reason: Option<&str>) -> Result<FreezeResponse> {
        let body = FreezeRequest { reason: reason.map(str::to_string) };
        self.call(Method::POST, paths::FREEZE.to_string(), Some(&body)).await
    }

    /// `POST /kernel/unfreeze`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn unfreeze(&self) -> Result<FreezeResponse> {
        self.call(Method::POST, paths::UNFREEZE.to_string(), Some(&())).await
    }

    /// `GET /kernel/events`: the event bus from just after `after`, then live.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached; a broken
    /// stream ends with the error as its last item.
    pub async fn events(
        &self,
        after: Option<u64>,
    ) -> Result<BoxStream<'static, Result<EventRecord>>> {
        let path = match after {
            Some(cursor) => format!("{}?after={cursor}", paths::EVENTS),
            None => paths::EVENTS.to_string(),
        };
        let answer = self.send(Method::GET, &path, None, None).await?;
        if !answer.status.is_success() {
            let status = answer.status;
            return Err(answer_error(status, &answer.bytes().await?));
        }
        Ok(server_sent_records(answer.body))
    }

    /// `POST /kernel/attach/{session}`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::ControlPlane`] when the kernel cannot be reached, or the
    /// error the kernel answered.
    pub async fn attach(
        &self,
        session_id: &str,
        request: &AttachRequest,
    ) -> Result<AttachResponse> {
        let path = paths::ATTACH.replace("{session}", &encode(session_id));
        self.call(Method::POST, path, Some(request)).await
    }

    async fn process_action(
        &self,
        route: &str,
        pid: &str,
        body: &impl Serialize,
    ) -> Result<Process> {
        let path = route.replace("{pid}", &encode(pid));
        let answer: ProcessResponse = self.call(Method::POST, path, Some(body)).await?;
        Ok(answer.process)
    }

    /// Sends a JSON call and decodes the JSON answer, or the error the kernel answered.
    async fn call<T: DeserializeOwned>(
        &self,
        method: Method,
        path: String,
        body: Option<&impl Serialize>,
    ) -> Result<T> {
        let body = body.map(serde_json::to_vec).transpose()?;
        let answer = self.send(method, &path, body, Some(CALL_TIMEOUT)).await?;
        let status = answer.status;
        let bytes = answer.bytes().await?;
        if !status.is_success() {
            return Err(answer_error(status, &bytes));
        }
        serde_json::from_slice(&bytes).map_err(|error| KernelError::ControlPlane {
            reason: format!(
                "the kernel at {} answered {path} with a body this client cannot read ({error}); \
                 are the CLI and the kernel the same version?",
                self.endpoint()
            ),
        })
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: Option<Duration>,
    ) -> Result<Answer> {
        let path = format!("{}{path}", paths::API_PREFIX);
        let unreachable = |error: &dyn std::fmt::Display| KernelError::ControlPlane {
            reason: format!(
                "could not reach the kernel at {}: {error}; start it with `adk-rust up`",
                self.endpoint()
            ),
        };
        match &self.transport {
            Transport::Http { base, client } => {
                let mut request = client.request(method, format!("{base}{path}"));
                if let Some(token) = &self.token {
                    request = request.bearer_auth(token);
                }
                if let Some(body) = body {
                    request = request.header("content-type", "application/json").body(body);
                }
                if let Some(timeout) = timeout {
                    request = request.timeout(timeout);
                }
                let response = request.send().await.map_err(|error| unreachable(&error))?;
                let status = StatusCode::from_u16(response.status().as_u16())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let body = response
                    .bytes_stream()
                    .map(|chunk| {
                        chunk.map_err(|error| KernelError::ControlPlane {
                            reason: format!("the kernel's answer broke off: {error}"),
                        })
                    })
                    .boxed();
                Ok(Answer { status, body })
            }
            Transport::Socket(socket) => {
                let send = socket_request(socket, method, &path, self.token.as_deref(), body);
                let answer = match timeout {
                    Some(timeout) => tokio::time::timeout(timeout, send)
                        .await
                        .map_err(|_| unreachable(&"the kernel did not answer in time"))?,
                    None => send.await,
                };
                answer.map_err(|error| unreachable(&error))
            }
        }
    }
}

#[cfg(unix)]
async fn connect_local_socket(path: PathBuf) -> Result<Option<ControlClient>> {
    match tokio::fs::symlink_metadata(&path).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(KernelError::ControlPlane {
                reason: format!("could not read the kernel socket {}: {error}", path.display()),
            });
        }
    }
    match tokio::net::UnixStream::connect(&path).await {
        Ok(_) => Ok(Some(ControlClient::socket(path))),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(KernelError::ControlPlane {
            reason: format!(
                "the kernel socket {} exists but cannot be opened: {error}; it admits only the \
                 OS user that runs the kernel",
                path.display()
            ),
        }),
    }
}

#[cfg(not(unix))]
async fn connect_local_socket(_path: PathBuf) -> Result<Option<ControlClient>> {
    Ok(None)
}

/// One HTTP/1.1 exchange over the socket at `socket`.
#[cfg(unix)]
async fn socket_request(
    socket: &Path,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Option<Vec<u8>>,
) -> std::result::Result<Answer, String> {
    use http_body_util::{BodyStream, Full};
    use hyper_util::rt::TokioIo;

    let stream =
        tokio::net::UnixStream::connect(socket).await.map_err(|error| error.to_string())?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|error| error.to_string())?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::debug!(error = %error, "kernel socket connection closed");
        }
    });
    let mut request =
        hyper::Request::builder().method(method).uri(path).header(hyper::header::HOST, "localhost");
    if let Some(token) = token {
        request = request.header(hyper::header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if body.is_some() {
        request = request.header(hyper::header::CONTENT_TYPE, "application/json");
    }
    let request = request
        .body(Full::new(Bytes::from(body.unwrap_or_default())))
        .map_err(|error| error.to_string())?;
    let response = sender.send_request(request).await.map_err(|error| error.to_string())?;
    let status = response.status();
    let body = BodyStream::new(response.into_body())
        .filter_map(|frame| async move {
            match frame {
                Ok(frame) => frame.into_data().ok().map(Ok),
                Err(error) => Some(Err(KernelError::ControlPlane {
                    reason: format!("the kernel's answer broke off: {error}"),
                })),
            }
        })
        .boxed();
    Ok(Answer { status, body })
}

#[cfg(not(unix))]
async fn socket_request(
    socket: &Path,
    _method: Method,
    _path: &str,
    _token: Option<&str>,
    _body: Option<Vec<u8>>,
) -> std::result::Result<Answer, String> {
    Err(format!(
        "the local control socket {} needs a Unix platform; the Windows named pipe is spec 3.2 \
         §10 W2, so reach the kernel over HTTP with ControlClient::http",
        socket.display()
    ))
}

/// The error a kernel's non-success answer carries: its problem document, the server's
/// `{"error": "..."}` refusal, or the status alone.
fn answer_error(status: StatusCode, body: &[u8]) -> KernelError {
    let value: Option<Value> = serde_json::from_slice(body).ok();
    let error = value.as_ref().and_then(|value| value.get("error"));
    if let Some(problem) = error.and_then(Value::as_object) {
        let message = problem.get("message").and_then(Value::as_str).unwrap_or("").to_string();
        let code = problem.get("code").and_then(Value::as_str).unwrap_or("");
        let category = problem
            .get("category")
            .and_then(|category| serde_json::from_value(category.clone()).ok())
            .unwrap_or_else(|| category_of(status));
        return KernelError::Core(AdkError::new(
            ErrorComponent::Server,
            category,
            known_code(code),
            message,
        ));
    }
    let message = match error.and_then(Value::as_str) {
        Some(message) => message.to_string(),
        None => String::from_utf8_lossy(body).trim().chars().take(300).collect(),
    };
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => KernelError::Unauthorized {
            reason: format!(
                "{message}; pass the kernel's bearer token (--token or ADK_SERVE_TOKEN), or run \
                 the command as the user that runs the kernel"
            ),
        },
        _ => {
            KernelError::ControlPlane { reason: format!("the kernel answered {status}: {message}") }
        }
    }
}

/// The category a status stands for, when an answer names none.
fn category_of(status: StatusCode) -> ErrorCategory {
    match status.as_u16() {
        400 => ErrorCategory::InvalidInput,
        401 => ErrorCategory::Unauthorized,
        403 => ErrorCategory::Forbidden,
        404 => ErrorCategory::NotFound,
        409 => ErrorCategory::Conflict,
        429 => ErrorCategory::RateLimited,
        501 => ErrorCategory::Unsupported,
        503 => ErrorCategory::Unavailable,
        _ => ErrorCategory::Internal,
    }
}

/// The kernel code `code` names, or the control-plane code for one this client does not know.
fn known_code(code: &str) -> &'static str {
    [
        error::NOT_FOUND_CODE,
        error::INVALID_STATE_CODE,
        error::FROZEN_CODE,
        error::LIFECYCLE_CODE,
        error::CONFIG_CODE,
        error::STORE_CODE,
        error::CONTROL_PLANE_CODE,
        error::UNAUTHORIZED_CODE,
        error::IO_CODE,
        error::SERDE_CODE,
    ]
    .into_iter()
    .find(|known| *known == code)
    .unwrap_or(error::CONTROL_PLANE_CODE)
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

/// The event records of a server-sent event stream: the `data` of each event, keep-alive
/// comments skipped.
fn server_sent_records(
    body: BoxStream<'static, Result<Bytes>>,
) -> BoxStream<'static, Result<EventRecord>> {
    struct Reader {
        body: BoxStream<'static, Result<Bytes>>,
        buffer: Vec<u8>,
        ready: VecDeque<Result<EventRecord>>,
        done: bool,
    }
    let reader = Reader { body, buffer: Vec::new(), ready: VecDeque::new(), done: false };
    stream::unfold(reader, |mut reader| async move {
        loop {
            if let Some(record) = reader.ready.pop_front() {
                return Some((record, reader));
            }
            if reader.done {
                return None;
            }
            match reader.body.next().await {
                Some(Ok(chunk)) => {
                    reader.buffer.extend_from_slice(&chunk);
                    while let Some(end) = event_end(&reader.buffer) {
                        let block: Vec<u8> = reader.buffer.drain(..end.0).collect();
                        reader.buffer.drain(..end.1);
                        if let Some(record) = parse_event(&block) {
                            reader.ready.push_back(record);
                        }
                    }
                }
                Some(Err(error)) => {
                    reader.done = true;
                    reader.ready.push_back(Err(error));
                }
                None => reader.done = true,
            }
        }
    })
    .boxed()
}

/// Where the first event in `buffer` ends: its length and the separator's.
fn event_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n").map(|at| (at, 2));
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n").map(|at| (at, 4));
    match (lf, crlf) {
        (Some(lf), Some(crlf)) => Some(if lf.0 <= crlf.0 { lf } else { crlf }),
        (found, None) | (None, found) => found,
    }
}

/// The record in one event's lines, or `None` for an event with no data.
fn parse_event(block: &[u8]) -> Option<Result<EventRecord>> {
    let text = String::from_utf8_lossy(block);
    let data: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| data.strip_prefix(' ').unwrap_or(data))
        .collect();
    if data.is_empty() {
        return None;
    }
    Some(serde_json::from_str(&data.join("\n")).map_err(KernelError::from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_percent_encoded() {
        assert_eq!(encode("run-1_a.b~c"), "run-1_a.b~c");
        assert_eq!(encode("a/b c?d"), "a%2Fb%20c%3Fd");
    }

    #[tokio::test]
    async fn server_sent_events_split_on_blank_lines_across_chunks() {
        let record = |cursor: u64| {
            serde_json::to_string(&EventRecord {
                cursor,
                at: chrono::Utc::now(),
                event: crate::KernelEvent::Governance { frozen: true, reason: None },
            })
            .unwrap()
        };
        let text = format!(
            ": keep-alive\n\nid: 1\nevent: governance\ndata: {}\n\nid: 2\r\ndata: {}\r\n\r\n",
            record(1),
            record(2)
        );
        let (head, tail) = text.split_at(40);
        let chunks = vec![Ok(Bytes::from(head.to_string())), Ok(Bytes::from(tail.to_string()))];
        let records: Vec<_> = server_sent_records(stream::iter(chunks).boxed())
            .map(|record| record.unwrap().cursor)
            .collect()
            .await;
        assert_eq!(records, vec![1, 2]);
    }

    #[test]
    fn answers_map_to_kernel_errors() {
        let problem = AdkError::from(KernelError::NotFound { pid: "run-9".into() });
        let body = serde_json::to_vec(&problem.to_problem_json()).unwrap();
        let error = AdkError::from(answer_error(StatusCode::NOT_FOUND, &body));
        assert_eq!((error.code, error.category), (error::NOT_FOUND_CODE, ErrorCategory::NotFound));
        assert!(error.message.contains("run-9"));

        let refused =
            answer_error(StatusCode::UNAUTHORIZED, br#"{"error":"missing authorization"}"#);
        assert!(matches!(refused, KernelError::Unauthorized { .. }), "{refused}");

        let other = answer_error(StatusCode::BAD_GATEWAY, b"upstream down");
        assert!(other.to_string().contains("upstream down"), "{other}");
    }
}
