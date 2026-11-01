//! An egress proxy that admits a sandboxed call's connections by an [`EgressPolicy`].
//!
//! A sandbox whose network access is `Proxied` reaches only the proxy's loopback port and
//! finds it through the standard proxy variables. The proxy decides every destination:
//!
//! | Request | Handling |
//! |---------|----------|
//! | `CONNECT host:port` | The name and port are checked, the proxy resolves the name itself and checks every address, and connects only to an address it checked, so a name cannot be rebound to another address between the check and the connection. The client's TLS `ClientHello` must name `host` in its SNI; a `ClientHello` with no SNI, with Encrypted Client Hello, or that is not TLS is refused. A `CONNECT` to an IP address literal is admitted only by the policy's address rules and needs no SNI |
//! | `GET http://host[:port]/path` (absolute form) | Only when the policy admits plain `http`; checked and resolved the same way, then forwarded with the `Host` header set to the checked authority and the hop-by-hop headers removed |
//! | Anything else | `400 Bad Request` |
//! | A loopback or unspecified destination | Always refused, whatever the policy says, so the proxy never connects to itself or to the host's local services |
//!
//! A refusal answers `403 Forbidden` with the reason and an `X-Adk-Egress-Denied` header naming
//! the [`EgressDenial::code`]. Every connection, admitted or refused, is one [`EgressRecord`]
//! for the call's [`EgressAudit`] and one `tracing` event. Start one proxy per sandboxed call,
//! so each record names its call.
//!
//! The proxy does not follow redirects; a redirect the client follows is a new request, checked
//! again. It is not data-loss prevention (an admitted host receives whatever is sent to it) and
//! not proof against domain fronting: a shared CDN can route an admitted name to another tenant.
//!
//! [`EgressProxyBuilder::start_for`] starts the proxy where a sandboxed call's enforcer needs it
//! and returns the [`CallProxy`] whose [`network`](CallProxy::network) the call's policy runs
//! under: on the host's loopback for Seatbelt, inside the call's network namespace for
//! bubblewrap.
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_core::egress::EgressPolicy;
//! use adk_sandbox::egress_proxy::{EgressProxy, InMemoryEgressAudit};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let audit = Arc::new(InMemoryEgressAudit::new());
//! let proxy = EgressProxy::builder(EgressPolicy::allowlist(["pypi.org:443", "*.pythonhosted.org:443"])?)
//!     .call_id("call-42")
//!     .audit(audit.clone())
//!     .bind()
//!     .await?;
//! // Run the sandboxed command with HTTPS_PROXY=http://127.0.0.1:<port>.
//! println!("proxy on 127.0.0.1:{}", proxy.port());
//! drop(proxy); // Stops the listener and every open connection.
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use adk_core::egress::{
    AddressClass, EgressDenial, EgressPolicy, EgressTarget, HostResolver, SystemResolver, classify,
};
use chrono::{DateTime, Utc};

use crate::sandbox::{NetworkAccess, ProxyEndpoint, ProxyListener, SandboxEnforcer};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// The largest request head the proxy reads.
const MAX_HEAD: usize = 16 * 1024;

/// The largest TLS `ClientHello` the proxy reads, across records.
const MAX_CLIENT_HELLO: usize = 64 * 1024;

/// Headers that apply to one hop and are not forwarded (RFC 9110 §7.6.1); `Host` is replaced
/// by the checked authority. `Transfer-Encoding` stays, since the body is relayed as is.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "proxy-connection",
    "keep-alive",
    "proxy-authorization",
    "proxy-authenticate",
    "te",
    "trailer",
    "upgrade",
    "host",
];

/// How a client asked the proxy for a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProxyProtocol {
    /// An HTTP `CONNECT` tunnel carrying TLS.
    Connect,
    /// A plain `http` request in absolute form.
    Http,
}

/// What became of one connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum EgressOutcome {
    /// The connection was admitted and carried until it closed.
    Allowed,
    /// The policy, or a proxy rule, refused it.
    Denied {
        /// A stable identifier such as `egress.host_not_allowed` or `egress.sni_mismatch`.
        code: String,
        /// The reason, as the client was told.
        reason: String,
    },
    /// It was admitted but could not be carried out, for example because the destination
    /// did not answer.
    Failed {
        /// What failed.
        reason: String,
    },
}

/// One connection through the proxy, for an audit trail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressRecord {
    /// The call the proxy serves ([`EgressProxyBuilder::call_id`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call: Option<String>,
    /// When the client connected.
    pub started_at: DateTime<Utc>,
    /// How the client asked.
    pub protocol: ProxyProtocol,
    /// The host the client asked for, as it was written.
    pub host: String,
    /// The port the client asked for.
    pub port: u16,
    /// The address the proxy connected to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<SocketAddr>,
    /// The server name of the client's TLS `ClientHello`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sni: Option<String>,
    /// What became of the connection.
    pub outcome: EgressOutcome,
    /// Bytes from the client to the destination.
    pub bytes_sent: u64,
    /// Bytes from the destination to the client.
    pub bytes_received: u64,
}

/// Receives every [`EgressRecord`] of a proxy.
///
/// Called on the proxy's connection tasks; keep it quick and non-blocking.
pub trait EgressAudit: Send + Sync {
    /// Takes one record.
    fn record(&self, record: EgressRecord);
}

/// An [`EgressAudit`] that keeps records in memory, for tests and short-lived calls.
#[derive(Debug, Default)]
pub struct InMemoryEgressAudit {
    records: Mutex<Vec<EgressRecord>>,
}

impl InMemoryEgressAudit {
    /// An empty audit trail.
    pub fn new() -> Self {
        Self::default()
    }

    /// The records so far, oldest first.
    pub fn records(&self) -> Vec<EgressRecord> {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl EgressAudit for InMemoryEgressAudit {
    fn record(&self, record: EgressRecord) {
        self.records.lock().unwrap_or_else(|e| e.into_inner()).push(record);
    }
}

/// Builds an [`EgressProxy`].
pub struct EgressProxyBuilder {
    policy: EgressPolicy,
    resolver: Arc<dyn HostResolver>,
    audit: Option<Arc<dyn EgressAudit>>,
    call: Option<String>,
    handshake_timeout: Duration,
    #[cfg(test)]
    upstream: Option<SocketAddr>,
}

impl fmt::Debug for EgressProxyBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EgressProxyBuilder")
            .field("policy", &self.policy.to_string())
            .field("resolver", &self.resolver)
            .field("audit", &self.audit.is_some())
            .field("call", &self.call)
            .field("handshake_timeout", &self.handshake_timeout)
            .finish()
    }
}

impl EgressProxyBuilder {
    /// Resolves names through `resolver` instead of the operating system.
    #[must_use]
    pub fn resolver(mut self, resolver: Arc<dyn HostResolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Sends every [`EgressRecord`] to `audit`.
    #[must_use]
    pub fn audit(mut self, audit: Arc<dyn EgressAudit>) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Names the call the proxy serves in every record and log line.
    #[must_use]
    pub fn call_id(mut self, call: impl Into<String>) -> Self {
        self.call = Some(call.into());
        self
    }

    /// Bounds reading the request, reading the TLS `ClientHello`, and connecting to the
    /// destination, each (default 10 seconds).
    #[must_use]
    pub fn handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Starts the proxy on a new listener at `127.0.0.1` on a free port.
    ///
    /// # Errors
    ///
    /// Returns the error binding the listener.
    pub async fn bind(self) -> std::io::Result<EgressProxy> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        self.start(listener)
    }

    /// Starts the proxy on `listener`, for a listener opened elsewhere, such as inside a
    /// sandbox's network namespace.
    ///
    /// # Errors
    ///
    /// Returns the error making the listener non-blocking or registering it with the runtime.
    pub fn serve(self, listener: std::net::TcpListener) -> std::io::Result<EgressProxy> {
        listener.set_nonblocking(true)?;
        self.start(TcpListener::from_std(listener)?)
    }

    /// Starts the proxy of one sandboxed call where `enforcer` needs it
    /// ([`SandboxEnforcer::proxy_listener`]) and returns it with the `Proxied` network access
    /// for the call's policy.
    ///
    /// On the host's loopback the proxy listens at once. Inside a sandbox's network namespace
    /// it starts when the enforcer hands over the listener it opened there, as the command
    /// built under [`CallProxy::network`] starts.
    ///
    /// # Errors
    ///
    /// Returns the error binding a host listener, or an error when no namespace port is free.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::ffi::{OsStr, OsString};
    /// use std::path::Path;
    ///
    /// use adk_core::egress::EgressPolicy;
    /// use adk_sandbox::egress_proxy::EgressProxy;
    /// use adk_sandbox::sandbox::{SandboxPolicyBuilder, get_enforcer, sandboxed_command};
    ///
    /// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
    /// let enforcer = get_enforcer()?;
    /// let proxy = EgressProxy::builder(EgressPolicy::allowlist(["pypi.org:443"])?)
    ///     .start_for(enforcer.as_ref())
    ///     .await?;
    /// let policy = SandboxPolicyBuilder::new()
    ///     .allow_read("/usr")
    ///     .network(proxy.network())
    ///     .env("PATH", "/usr/bin:/bin")
    ///     .build();
    /// let output = sandboxed_command(
    ///     enforcer.as_ref(),
    ///     OsStr::new("curl"),
    ///     &[OsString::from("-sI"), OsString::from("https://pypi.org/")],
    ///     &policy,
    ///     Path::new("/tmp"),
    /// )?
    /// .output()
    /// .await?;
    /// drop(proxy); // after the command ends
    /// # Ok(())
    /// # }
    /// ```
    pub async fn start_for(self, enforcer: &dyn SandboxEnforcer) -> std::io::Result<CallProxy> {
        match enforcer.proxy_listener() {
            ProxyListener::HostLoopback => {
                let proxy = self.bind().await?;
                let port = proxy.port();
                Ok(CallProxy {
                    network: NetworkAccess::Proxied { endpoint: ProxyEndpoint { port } },
                    proxy: Arc::new(Mutex::new(Some(proxy))),
                    reservation: None,
                })
            }
            ProxyListener::SandboxLoopback => {
                let (port, id, listener) = reserve_namespace_port()?;
                let proxy = Arc::new(Mutex::new(None));
                let slot = Arc::clone(&proxy);
                tokio::spawn(async move {
                    let Ok(listener) = listener.await else { return };
                    match self.serve(listener) {
                        Ok(started) => {
                            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(started)
                        }
                        Err(error) => tracing::warn!(
                            error = %error,
                            "the egress proxy could not serve the sandbox's listener"
                        ),
                    }
                });
                Ok(CallProxy {
                    network: NetworkAccess::Proxied { endpoint: ProxyEndpoint { port } },
                    proxy,
                    reservation: Some((port, id)),
                })
            }
        }
    }

    fn start(self, listener: TcpListener) -> std::io::Result<EgressProxy> {
        let local = listener.local_addr()?;
        let shared = Arc::new(Shared {
            policy: self.policy,
            resolver: self.resolver,
            audit: self.audit,
            call: self.call,
            handshake_timeout: self.handshake_timeout,
            #[cfg(test)]
            upstream: self.upstream,
        });
        let task = tokio::spawn(async move {
            // Dropping the set aborts every connection when the proxy stops.
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((client, _)) => {
                            connections.spawn(Arc::clone(&shared).handle(client));
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "egress proxy accept failed");
                        }
                    },
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Ok(EgressProxy { local, task })
    }
}

/// A running egress proxy; dropping it stops the listener and every open connection.
///
/// See the [module documentation](self).
#[derive(Debug)]
pub struct EgressProxy {
    local: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl EgressProxy {
    /// Starts a builder enforcing `policy`.
    pub fn builder(policy: EgressPolicy) -> EgressProxyBuilder {
        EgressProxyBuilder {
            policy,
            resolver: Arc::new(SystemResolver),
            audit: None,
            call: None,
            handshake_timeout: Duration::from_secs(10),
            #[cfg(test)]
            upstream: None,
        }
    }

    /// The listener's address.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// The listener's port.
    pub fn port(&self) -> u16 {
        self.local.port()
    }
}

impl Drop for EgressProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The egress proxy of one sandboxed call ([`EgressProxyBuilder::start_for`]); dropping it
/// stops the proxy and every open connection.
#[derive(Debug)]
pub struct CallProxy {
    network: NetworkAccess,
    proxy: Arc<Mutex<Option<EgressProxy>>>,
    // The namespace port and the id of this call's reservation of it.
    reservation: Option<(u16, u64)>,
}

impl CallProxy {
    /// The `Proxied` network access the call's [`SandboxPolicy`](crate::SandboxPolicy) runs
    /// under.
    pub fn network(&self) -> NetworkAccess {
        self.network
    }

    /// Whether the proxy is listening: at once on the host's loopback, and inside a sandbox's
    /// namespace once its command has started.
    pub fn is_listening(&self) -> bool {
        self.proxy.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }
}

impl Drop for CallProxy {
    fn drop(&mut self) {
        if let Some((port, id)) = self.reservation {
            let mut ports = namespace_ports().lock().unwrap_or_else(|e| e.into_inner());
            // A port delivered and reserved again belongs to another call by now.
            if ports.get(&port).is_some_and(|(owner, _)| *owner == id) {
                ports.remove(&port);
            }
        }
    }
}

type NamespacePorts =
    Mutex<HashMap<u16, (u64, tokio::sync::oneshot::Sender<std::net::TcpListener>)>>;

/// The calls waiting for a listener inside their sandbox's namespace, by the port each one
/// listens on there. Every sandbox has its own namespace; the ports only tell this process's
/// waiting calls apart.
fn namespace_ports() -> &'static NamespacePorts {
    static PORTS: OnceLock<NamespacePorts> = OnceLock::new();
    PORTS.get_or_init(NamespacePorts::default)
}

/// Reserves a namespace port; returns it, the reservation's id, and where the listener arrives.
fn reserve_namespace_port()
-> std::io::Result<(u16, u64, tokio::sync::oneshot::Receiver<std::net::TcpListener>)> {
    use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
    const FIRST: u16 = 20_000;
    const COUNT: u16 = 40_000;
    static NEXT_PORT: AtomicU16 = AtomicU16::new(0);
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let mut ports = namespace_ports().lock().unwrap_or_else(|e| e.into_inner());
    for _ in 0..COUNT {
        let port = FIRST + NEXT_PORT.fetch_add(1, Ordering::Relaxed) % COUNT;
        if let std::collections::hash_map::Entry::Vacant(slot) = ports.entry(port) {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let (sender, receiver) = tokio::sync::oneshot::channel();
            slot.insert((id, sender));
            return Ok((port, id, receiver));
        }
    }
    Err(std::io::Error::other("every egress proxy namespace port is in use"))
}

/// Hands the listener an enforcer opened inside a sandbox's namespace to the call waiting for
/// `port`; `false` when none waits, and the listener is closed.
#[cfg_attr(not(all(target_os = "linux", feature = "sandbox-linux")), allow(dead_code))]
pub(crate) fn deliver_namespace_listener(port: u16, listener: std::net::TcpListener) -> bool {
    let waiting = namespace_ports().lock().unwrap_or_else(|e| e.into_inner()).remove(&port);
    waiting.is_some_and(|(_, sender)| sender.send(listener).is_ok())
}

struct Shared {
    policy: EgressPolicy,
    resolver: Arc<dyn HostResolver>,
    audit: Option<Arc<dyn EgressAudit>>,
    call: Option<String>,
    handshake_timeout: Duration,
    // Tests run destinations on loopback, which the proxy refuses; this carries a checked
    // public address to the test's listener instead.
    #[cfg(test)]
    upstream: Option<SocketAddr>,
}

/// Why a connection was not carried.
enum Refusal {
    Denied { code: &'static str, reason: String },
    Failed(String),
    BadRequest(String),
}

impl From<EgressDenial> for Refusal {
    fn from(denial: EgressDenial) -> Self {
        Self::Denied { code: denial.code(), reason: denial.to_string() }
    }
}

impl Shared {
    async fn handle(self: Arc<Self>, mut client: TcpStream) {
        let mut record = EgressRecord {
            call: self.call.clone(),
            started_at: Utc::now(),
            protocol: ProxyProtocol::Connect,
            host: String::new(),
            port: 0,
            address: None,
            sni: None,
            outcome: EgressOutcome::Allowed,
            bytes_sent: 0,
            bytes_received: 0,
        };
        let mut answered = false;
        let result = self.carry(&mut client, &mut record, &mut answered).await;
        if let Err(refusal) = result {
            let (status, outcome) = match refusal {
                Refusal::Denied { code, reason } => {
                    ("403 Forbidden", EgressOutcome::Denied { code: code.to_string(), reason })
                }
                Refusal::Failed(reason) => ("502 Bad Gateway", EgressOutcome::Failed { reason }),
                Refusal::BadRequest(reason) => (
                    "400 Bad Request",
                    EgressOutcome::Denied { code: "egress.bad_request".to_string(), reason },
                ),
            };
            // A CONNECT tunnel already answered `200` carries no second status line.
            if !answered {
                let (code, reason) = match &outcome {
                    EgressOutcome::Denied { code, reason } => (code.as_str(), reason.as_str()),
                    EgressOutcome::Failed { reason } => ("egress.upstream_failed", reason.as_str()),
                    EgressOutcome::Allowed => ("", ""),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nX-Adk-Egress-Denied: {code}\r\nContent-Type: \
                     text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n\
                     {reason}",
                    reason.len()
                );
                let _ = client.write_all(response.as_bytes()).await;
            }
            record.outcome = outcome;
        }
        match &record.outcome {
            EgressOutcome::Allowed => tracing::info!(
                egress.call = record.call.as_deref().unwrap_or_default(),
                egress.host = %record.host,
                egress.port = record.port,
                egress.address = ?record.address,
                egress.bytes_sent = record.bytes_sent,
                egress.bytes_received = record.bytes_received,
                "egress proxy connection closed"
            ),
            EgressOutcome::Denied { code, reason } => tracing::warn!(
                egress.call = record.call.as_deref().unwrap_or_default(),
                egress.host = %record.host,
                egress.port = record.port,
                egress.code = %code,
                reason = %reason,
                "egress proxy refused a connection"
            ),
            EgressOutcome::Failed { reason } => tracing::warn!(
                egress.call = record.call.as_deref().unwrap_or_default(),
                egress.host = %record.host,
                egress.port = record.port,
                reason = %reason,
                "egress proxy connection failed"
            ),
        }
        if let Some(audit) = &self.audit {
            audit.record(record);
        }
    }

    async fn carry(
        &self,
        client: &mut TcpStream,
        record: &mut EgressRecord,
        answered: &mut bool,
    ) -> Result<(), Refusal> {
        let mut buffer = Vec::with_capacity(1024);
        let head_end = self.within(read_head(client, &mut buffer)).await??;
        let head = std::str::from_utf8(&buffer[..head_end])
            .map_err(|_| Refusal::BadRequest("the request head is not UTF-8".to_string()))?;
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let mut parts = request_line.split(' ');
        let (Some(method), Some(target), Some(version), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(Refusal::BadRequest(format!("malformed request line '{request_line}'")));
        };
        let rest = buffer[head_end + 4..].to_vec();
        if method.eq_ignore_ascii_case("CONNECT") {
            record.protocol = ProxyProtocol::Connect;
            let (host, port) = split_authority(target, None)?;
            (record.host, record.port) = (host.clone(), port);
            let addresses = self.admit(&host, port).await?;
            client
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .await
                .map_err(|e| Refusal::Failed(format!("the client went away: {e}")))?;
            *answered = true;
            let mut hello = rest;
            record.sni = match self.within(read_client_hello(client, &mut hello)).await?? {
                ClientHello::Sni(name) => Some(name),
                ClientHello::NoSni => None,
                ClientHello::Ech => {
                    return Err(denied(
                        "egress.ech",
                        "Encrypted Client Hello hides the server name; turn ECH off for \
                         proxied connections",
                    ));
                }
                ClientHello::NotTls => {
                    return Err(denied(
                        "egress.not_tls",
                        "a CONNECT tunnel carries TLS only; use an http:// URL for plain HTTP",
                    ));
                }
            };
            // An address literal is admitted by the policy's address rules; a TLS server name
            // is never an address, so only a named host is matched against the SNI.
            if parse_literal(&host).is_none() {
                match record.sni.as_deref() {
                    None => {
                        return Err(denied(
                            "egress.no_sni",
                            &format!("the TLS ClientHello names no server; it must name {host}"),
                        ));
                    }
                    Some(sni) if normalize(sni) != normalize(&host) => {
                        return Err(denied(
                            "egress.sni_mismatch",
                            &format!(
                                "the TLS ClientHello names '{sni}', not the CONNECT host '{host}'"
                            ),
                        ));
                    }
                    Some(_) => {}
                }
            }
            let mut upstream = self.connect(&addresses, record).await?;
            upstream
                .write_all(&hello)
                .await
                .map_err(|e| Refusal::Failed(format!("the destination went away: {e}")))?;
            record.bytes_sent = hello.len() as u64;
            self.splice(client, &mut upstream, record).await;
            return Ok(());
        }
        record.protocol = ProxyProtocol::Http;
        let Some(after_scheme) = target.strip_prefix("http://") else {
            return Err(Refusal::BadRequest(format!(
                "'{method} {target}' is not a proxy request; send CONNECT for https and an \
                 absolute http:// URL for plain HTTP"
            )));
        };
        let (authority, path) = match after_scheme.find(['/', '?']) {
            Some(index) => (&after_scheme[..index], &after_scheme[index..]),
            None => (after_scheme, "/"),
        };
        let path = if path.starts_with('?') { format!("/{path}") } else { path.to_string() };
        let (host, port) = split_authority(authority, Some(80))?;
        (record.host, record.port) = (host.clone(), port);
        if !self.policy.allows_plain_http() {
            return Err(EgressDenial::PlainHttp { host }.into());
        }
        let addresses = self.admit(&host, port).await?;
        let mut forwarded = format!("{method} {path} {version}\r\nHost: {authority}\r\n");
        for line in lines.filter(|line| !line.is_empty()) {
            let name = line.split(':').next().unwrap_or_default().trim().to_ascii_lowercase();
            if !HOP_BY_HOP.contains(&name.as_str()) {
                forwarded.push_str(line);
                forwarded.push_str("\r\n");
            }
        }
        forwarded.push_str("Connection: close\r\n\r\n");
        let mut upstream = self.connect(&addresses, record).await?;
        let mut first = forwarded.into_bytes();
        first.extend_from_slice(&rest);
        upstream
            .write_all(&first)
            .await
            .map_err(|e| Refusal::Failed(format!("the destination went away: {e}")))?;
        record.bytes_sent = first.len() as u64;
        self.splice(client, &mut upstream, record).await;
        Ok(())
    }

    /// Checks the name or literal and every address the name resolves to, and returns the
    /// addresses to connect to: only addresses that passed.
    async fn admit(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, Refusal> {
        self.policy.decide(&EgressTarget::Name { host, port })?;
        let addresses = match parse_literal(host) {
            Some(ip) => vec![ip],
            None => self
                .within(self.resolver.resolve(host))
                .await?
                .map_err(|e| Refusal::Failed(format!("{host} did not resolve: {e}")))?,
        };
        if addresses.is_empty() {
            return Err(Refusal::Failed(format!("{host} resolves to no address")));
        }
        for &address in &addresses {
            if matches!(classify(address), AddressClass::Loopback | AddressClass::Unspecified) {
                return Err(denied(
                    "egress.loopback",
                    &format!(
                        "{host} reaches {address}, a loopback or unspecified address, which the \
                         egress proxy never connects to"
                    ),
                ));
            }
            self.policy.decide(&EgressTarget::Address { host, address })?;
        }
        Ok(addresses.into_iter().map(|address| SocketAddr::new(address, port)).collect())
    }

    async fn connect(
        &self,
        addresses: &[SocketAddr],
        record: &mut EgressRecord,
    ) -> Result<TcpStream, Refusal> {
        let mut last = String::new();
        for &address in addresses {
            #[cfg(test)]
            let target = self.upstream.unwrap_or(address);
            #[cfg(not(test))]
            let target = address;
            match self.within(TcpStream::connect(target)).await? {
                Ok(stream) => {
                    record.address = Some(address);
                    return Ok(stream);
                }
                Err(error) => last = format!("{address}: {error}"),
            }
        }
        Err(Refusal::Failed(format!("no address of {} answered ({last})", record.host)))
    }

    async fn splice(
        &self,
        client: &mut TcpStream,
        upstream: &mut TcpStream,
        record: &mut EgressRecord,
    ) {
        match tokio::io::copy_bidirectional(client, upstream).await {
            Ok((sent, received)) => {
                record.bytes_sent += sent;
                record.bytes_received = received;
            }
            Err(error) => {
                tracing::debug!(error = %error, "egress proxy connection ended with an error")
            }
        }
    }

    async fn within<T>(&self, future: impl std::future::Future<Output = T>) -> Result<T, Refusal> {
        tokio::time::timeout(self.handshake_timeout, future)
            .await
            .map_err(|_| Refusal::Failed(format!("timed out after {:?}", self.handshake_timeout)))
    }
}

fn denied(code: &'static str, reason: &str) -> Refusal {
    Refusal::Denied { code, reason: reason.to_string() }
}

fn normalize(host: &str) -> String {
    host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase()
}

fn parse_literal(host: &str) -> Option<IpAddr> {
    host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host).parse().ok()
}

/// Splits `host:port`, or `[v6]:port`; `default` supplies a missing port.
fn split_authority(authority: &str, default: Option<u16>) -> Result<(String, u16), Refusal> {
    let bad = || Refusal::BadRequest(format!("'{authority}' is not host:port"));
    if authority.contains('@') {
        return Err(Refusal::BadRequest("a proxy target carries no credentials".to_string()));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') || host.ends_with(']') => (host, Some(port)),
        _ => (authority, None),
    };
    let port = match (port, default) {
        (Some(port), _) => port.parse::<u16>().ok().filter(|port| *port != 0).ok_or_else(bad)?,
        (None, Some(default)) => default,
        (None, None) => return Err(bad()),
    };
    if host.is_empty() {
        return Err(bad());
    }
    Ok((host.to_string(), port))
}

/// Reads until the end of the request head; returns the index of its `\r\n\r\n`.
async fn read_head(client: &mut TcpStream, buffer: &mut Vec<u8>) -> Result<usize, Refusal> {
    let mut chunk = [0u8; 2048];
    loop {
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            return Ok(end);
        }
        if buffer.len() > MAX_HEAD {
            return Err(Refusal::BadRequest(format!("the request head exceeds {MAX_HEAD} bytes")));
        }
        match client.read(&mut chunk).await {
            Ok(0) => return Err(Refusal::BadRequest("the client closed before a request".into())),
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(error) => return Err(Refusal::Failed(format!("reading the request: {error}"))),
        }
    }
}

/// What a TLS `ClientHello` says about its destination.
#[derive(Debug, PartialEq, Eq)]
enum ClientHello {
    Sni(String),
    NoSni,
    Ech,
    NotTls,
}

/// Reads until `buffer` holds a whole `ClientHello`, which may span TLS records.
async fn read_client_hello(
    client: &mut TcpStream,
    buffer: &mut Vec<u8>,
) -> Result<ClientHello, Refusal> {
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(hello) = parse_client_hello(buffer) {
            return Ok(hello);
        }
        if buffer.len() > MAX_CLIENT_HELLO {
            return Err(denied("egress.not_tls", "the TLS ClientHello is too large"));
        }
        match client.read(&mut chunk).await {
            Ok(0) => {
                return Err(Refusal::Failed("the client closed before its ClientHello".into()));
            }
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(error) => return Err(Refusal::Failed(format!("reading the ClientHello: {error}"))),
        }
    }
}

/// Parses the `ClientHello` at the start of `data`; `None` until it is complete.
fn parse_client_hello(data: &[u8]) -> Option<ClientHello> {
    let mut handshake = Vec::new();
    let mut position = 0;
    while handshake.len() < 4 || handshake.len() < 4 + u24(&handshake[1..4]) {
        let header = data.get(position..position + 5)?;
        if header[0] != 0x16 {
            return Some(ClientHello::NotTls);
        }
        let length = usize::from(u16::from_be_bytes([header[3], header[4]]));
        handshake.extend_from_slice(data.get(position + 5..position + 5 + length)?);
        position += 5 + length;
        if handshake.first().is_some_and(|kind| *kind != 1) {
            return Some(ClientHello::NotTls);
        }
    }
    let body = &handshake[4..4 + u24(&handshake[1..4])];
    Some(client_hello_body(body).unwrap_or(ClientHello::NotTls))
}

fn u24(bytes: &[u8]) -> usize {
    (usize::from(bytes[0]) << 16) | (usize::from(bytes[1]) << 8) | usize::from(bytes[2])
}

/// The server name and Encrypted Client Hello extensions of a `ClientHello` body; `None` when
/// it is malformed.
fn client_hello_body(body: &[u8]) -> Option<ClientHello> {
    let mut at = 2 + 32; // legacy_version, random
    let session = usize::from(*body.get(at)?);
    at += 1 + session;
    let suites = usize::from(u16::from_be_bytes([*body.get(at)?, *body.get(at + 1)?]));
    at += 2 + suites;
    let compression = usize::from(*body.get(at)?);
    at += 1 + compression;
    if at == body.len() {
        return Some(ClientHello::NoSni);
    }
    let total = usize::from(u16::from_be_bytes([*body.get(at)?, *body.get(at + 1)?]));
    let extensions = body.get(at + 2..at + 2 + total)?;
    let (mut at, mut sni, mut ech) = (0, None, false);
    while at < extensions.len() {
        let kind = u16::from_be_bytes([*extensions.get(at)?, *extensions.get(at + 1)?]);
        let length =
            usize::from(u16::from_be_bytes([*extensions.get(at + 2)?, *extensions.get(at + 3)?]));
        let data = extensions.get(at + 4..at + 4 + length)?;
        match kind {
            0x0000 => {
                // server_name_list: a u16 length, then entries of type, u16 length, name.
                let list = data.get(2..)?;
                if *list.first()? == 0 {
                    let name_length =
                        usize::from(u16::from_be_bytes([*list.get(1)?, *list.get(2)?]));
                    let name = list.get(3..3 + name_length)?;
                    sni = Some(String::from_utf8(name.to_vec()).ok()?);
                }
            }
            0xfe0d => ech = true,
            _ => {}
        }
        at += 4 + length;
    }
    Some(match (ech, sni) {
        (true, _) => ClientHello::Ech,
        (false, Some(name)) => ClientHello::Sni(name),
        (false, None) => ClientHello::NoSni,
    })
}

#[cfg(test)]
mod tests {
    use adk_core::egress::StaticResolver;

    use super::*;

    /// A TLS 1.3-style `ClientHello` record with an optional SNI and ECH extension.
    fn client_hello(sni: Option<&str>, ech: bool) -> Vec<u8> {
        let mut extensions = Vec::new();
        if let Some(name) = sni {
            let name = name.as_bytes();
            let entry_length = 3 + name.len();
            extensions.extend_from_slice(&0u16.to_be_bytes());
            extensions.extend_from_slice(&((2 + entry_length) as u16).to_be_bytes());
            extensions.extend_from_slice(&(entry_length as u16).to_be_bytes());
            extensions.push(0);
            extensions.extend_from_slice(&(name.len() as u16).to_be_bytes());
            extensions.extend_from_slice(name);
        }
        if ech {
            extensions.extend_from_slice(&0xfe0du16.to_be_bytes());
            extensions.extend_from_slice(&4u16.to_be_bytes());
            extensions.extend_from_slice(&[0, 0, 0, 0]);
        }
        let mut body = vec![0x03, 0x03];
        body.extend_from_slice(&[7u8; 32]);
        body.push(0); // session id
        body.extend_from_slice(&[0, 2, 0x13, 0x01]); // one cipher suite
        body.extend_from_slice(&[1, 0]); // null compression
        body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        body.extend_from_slice(&extensions);
        let mut handshake = vec![1];
        handshake.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        handshake.extend_from_slice(&body);
        let mut record = vec![0x16, 0x03, 0x01];
        record.extend_from_slice(&(handshake.len() as u16).to_be_bytes());
        record.extend_from_slice(&handshake);
        record
    }

    /// A destination that records what it receives and answers `pong`.
    async fn destination() -> (SocketAddr, Arc<Mutex<Vec<u8>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&received);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let sink = Arc::clone(&sink);
                tokio::spawn(async move {
                    let mut buffer = [0u8; 4096];
                    while let Ok(n) = stream.read(&mut buffer).await {
                        if n == 0 {
                            break;
                        }
                        sink.lock().unwrap().extend_from_slice(&buffer[..n]);
                        if stream.write_all(b"pong").await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        (address, received)
    }

    struct Fixture {
        proxy: EgressProxy,
        audit: Arc<InMemoryEgressAudit>,
        received: Arc<Mutex<Vec<u8>>>,
    }

    async fn fixture(policy: EgressPolicy) -> Fixture {
        let (upstream, received) = destination().await;
        let resolver = StaticResolver::new()
            .with_host("api.example.com", ["203.0.113.10".parse().unwrap()])
            .with_host("rebound.example.com", ["10.0.0.7".parse().unwrap()])
            .with_host(
                "mixed.example.com",
                ["203.0.113.11".parse().unwrap(), "127.0.0.1".parse().unwrap()],
            )
            .with_host("metadata.example.com", ["169.254.169.254".parse().unwrap()]);
        let audit = Arc::new(InMemoryEgressAudit::new());
        let mut builder = EgressProxy::builder(policy)
            .resolver(Arc::new(resolver))
            .audit(audit.clone())
            .call_id("call-1")
            .handshake_timeout(Duration::from_secs(2));
        builder.upstream = Some(upstream);
        Fixture { proxy: builder.bind().await.unwrap(), audit, received }
    }

    /// Sends `request`, then `after` once the head is answered; returns the head and the rest.
    async fn exchange(proxy: &EgressProxy, request: &str, after: &[u8]) -> (String, Vec<u8>) {
        let mut stream = TcpStream::connect(proxy.local_addr()).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let mut buffer = [0u8; 4096];
        let _ = tokio::time::timeout(Duration::from_millis(1000), async {
            while !response.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => response.extend_from_slice(&buffer[..n]),
                }
            }
        })
        .await;
        let end =
            response.windows(4).position(|w| w == b"\r\n\r\n").map_or(response.len(), |i| i + 4);
        let head = String::from_utf8_lossy(&response[..end]).into_owned();
        let mut rest = response[end..].to_vec();
        if head.starts_with("HTTP/1.1 200") && !after.is_empty() {
            stream.write_all(after).await.unwrap();
        }
        let _ = tokio::time::timeout(Duration::from_millis(500), async {
            while let Ok(n) = stream.read(&mut buffer).await {
                if n == 0 {
                    break;
                }
                rest.extend_from_slice(&buffer[..n]);
                if rest.ends_with(b"pong") {
                    break;
                }
            }
        })
        .await;
        (head, rest)
    }

    fn connect_to(host: &str) -> String {
        format!("CONNECT {host} HTTP/1.1\r\nHost: {host}\r\n\r\n")
    }

    /// Waits for the proxy to file its records for `count` connections.
    async fn records(audit: &InMemoryEgressAudit, count: usize) -> Vec<EgressRecord> {
        for _ in 0..100 {
            if audit.records().len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        audit.records()
    }

    #[tokio::test]
    async fn an_admitted_connect_carries_tls_to_the_checked_address() {
        let f = fixture(EgressPolicy::allowlist(["api.example.com:443"]).unwrap()).await;
        let hello = client_hello(Some("api.example.com"), false);

        let (head, rest) = exchange(&f.proxy, &connect_to("api.example.com:443"), &hello).await;

        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        assert_eq!(rest, b"pong");
        assert_eq!(*f.received.lock().unwrap(), hello);
        let records = records(&f.audit, 1).await;
        assert_eq!(records.len(), 1, "{records:?}");
        let record = &records[0];
        assert_eq!(
            (record.call.as_deref(), record.host.as_str(), record.port, record.sni.as_deref()),
            (Some("call-1"), "api.example.com", 443, Some("api.example.com"))
        );
        assert_eq!(record.address, Some("203.0.113.10:443".parse().unwrap()));
        assert_eq!(record.outcome, EgressOutcome::Allowed);
    }

    #[tokio::test]
    async fn refused_destinations_never_reach_the_network() {
        let policy = EgressPolicy::allowlist(["api.example.com:443", "*.example.com:443"])
            .unwrap()
            .with_private_networks();
        let f = fixture(policy).await;
        let cases = [
            ("other.test:443", "egress.host_not_allowed"),
            ("api.example.com:22", "egress.host_not_allowed"),
            ("mixed.example.com:443", "egress.loopback"),
            ("metadata.example.com:443", "egress.metadata_address"),
            ("203.0.113.10:443", "egress.literal_not_allowed"),
            ("[::1]:443", "egress.literal_not_allowed"),
            ("169.254.169.254:80", "egress.metadata_address"),
            ("0x7f.1:443", "egress.ambiguous_host"),
        ];
        for (target, code) in cases {
            let (head, _) = exchange(&f.proxy, &connect_to(target), &[]).await;
            assert!(head.starts_with("HTTP/1.1 403"), "{target}: {head}");
            assert!(head.contains(&format!("X-Adk-Egress-Denied: {code}")), "{target}: {head}");
        }
        assert!(f.received.lock().unwrap().is_empty(), "no destination was contacted");
        let records = records(&f.audit, cases.len()).await;
        assert!(records.iter().all(|r| matches!(r.outcome, EgressOutcome::Denied { .. })));
    }

    #[tokio::test]
    async fn a_name_resolving_to_a_private_address_is_refused() {
        let f = fixture(EgressPolicy::allowlist(["*.example.com:443"]).unwrap()).await;
        let (head, _) = exchange(&f.proxy, &connect_to("rebound.example.com:443"), &[]).await;
        assert!(head.contains("X-Adk-Egress-Denied: egress.address_not_allowed"), "{head}");
        assert!(f.received.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_tls_server_name_must_be_the_connect_host() {
        let f = fixture(EgressPolicy::allowlist(["api.example.com:443"]).unwrap()).await;
        let cases = [
            (client_hello(Some("evil.test"), false), "egress.sni_mismatch"),
            (client_hello(None, false), "egress.no_sni"),
            (client_hello(Some("api.example.com"), true), "egress.ech"),
            (b"GET / HTTP/1.1\r\n\r\n".to_vec(), "egress.not_tls"),
        ];
        for (hello, _) in &cases {
            let (head, rest) = exchange(&f.proxy, &connect_to("api.example.com:443"), hello).await;
            assert!(head.starts_with("HTTP/1.1 200"), "{head}");
            assert!(rest.is_empty(), "the tunnel closed without data");
        }
        assert!(f.received.lock().unwrap().is_empty(), "no destination was contacted");
        let codes: Vec<String> = records(&f.audit, cases.len())
            .await
            .into_iter()
            .map(|record| match record.outcome {
                EgressOutcome::Denied { code, .. } => code,
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(codes, cases.map(|(_, code)| code.to_string()));
    }

    /// curl's own `ClientHello` through the tunnel: the proxy reads its server name and passes
    /// the bytes to the checked address untouched.
    #[tokio::test]
    async fn a_real_tls_client_hello_names_its_server() {
        if std::process::Command::new("curl").arg("--version").output().is_err() {
            eprintln!("curl is not installed, skipping");
            return;
        }
        let f = fixture(EgressPolicy::allowlist(["api.example.com:443"]).unwrap()).await;
        let proxy = format!("http://127.0.0.1:{}", f.proxy.port());
        let status = tokio::process::Command::new("curl")
            .args(["-s", "-o", "/dev/null", "--max-time", "3", "--proxy", &proxy])
            .arg("https://api.example.com/")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .status()
            .await
            .expect("curl runs");
        assert!(!status.success(), "the fake destination speaks no TLS");

        let records = records(&f.audit, 1).await;
        assert_eq!(records[0].sni.as_deref(), Some("api.example.com"), "{records:?}");
        assert_eq!(records[0].address, Some("203.0.113.10:443".parse().unwrap()));
        let received = f.received.lock().unwrap().clone();
        assert_eq!(received.first(), Some(&0x16), "the ClientHello reached the destination");
        assert_eq!(
            parse_client_hello(&received),
            Some(ClientHello::Sni("api.example.com".to_string()))
        );
    }

    #[tokio::test]
    async fn a_fragmented_client_hello_is_reassembled() {
        let hello = client_hello(Some("api.example.com"), false);
        // Split the handshake across two records.
        let handshake = &hello[5..];
        let (first, second) = handshake.split_at(10);
        let mut fragmented = Vec::new();
        for part in [first, second] {
            fragmented.extend_from_slice(&[0x16, 0x03, 0x01]);
            fragmented.extend_from_slice(&(part.len() as u16).to_be_bytes());
            fragmented.extend_from_slice(part);
        }
        assert_eq!(
            parse_client_hello(&fragmented),
            Some(ClientHello::Sni("api.example.com".into()))
        );
        assert_eq!(parse_client_hello(&fragmented[..fragmented.len() - 1]), None);
    }

    #[tokio::test]
    async fn plain_http_is_forwarded_only_when_the_policy_admits_it() {
        let request = "GET http://api.example.com/a?b=1 HTTP/1.1\r\nHost: evil.test\r\n\
                       Proxy-Connection: keep-alive\r\nAccept: */*\r\n\r\n";
        let f = fixture(EgressPolicy::allowlist(["api.example.com:80"]).unwrap()).await;
        let (head, _) = exchange(&f.proxy, request, &[]).await;
        assert!(head.contains("X-Adk-Egress-Denied: egress.plain_http"), "{head}");
        assert!(f.received.lock().unwrap().is_empty());

        let f = fixture(EgressPolicy::allowlist(["api.example.com:80"]).unwrap().with_plain_http())
            .await;
        let (head, _) = exchange(&f.proxy, request, &[]).await;
        assert_eq!(head, "pong", "the destination's answer comes back as is");
        let forwarded = String::from_utf8(f.received.lock().unwrap().clone()).unwrap();
        assert_eq!(
            forwarded,
            "GET /a?b=1 HTTP/1.1\r\nHost: api.example.com\r\nAccept: */*\r\nConnection: close\r\n\r\n"
        );
    }

    #[tokio::test]
    async fn other_requests_are_bad_requests() {
        let f = fixture(EgressPolicy::public_only()).await;
        for request in [
            "GET /index.html HTTP/1.1\r\nHost: api.example.com\r\n\r\n",
            "GET https://api.example.com/ HTTP/1.1\r\n\r\n",
            "CONNECT user@api.example.com:443 HTTP/1.1\r\n\r\n",
            "CONNECT api.example.com HTTP/1.1\r\n\r\n",
            "NOT A REQUEST\r\n\r\n",
        ] {
            let (head, _) = exchange(&f.proxy, request, &[]).await;
            assert!(head.starts_with("HTTP/1.1 400"), "{request:?}: {head}");
        }
    }

    #[test]
    fn authorities_split_into_host_and_port() {
        let ok = |a: &str, d| split_authority(a, d).ok();
        assert_eq!(ok("api.example.com:443", None), Some(("api.example.com".into(), 443)));
        assert_eq!(ok("[::1]:8443", None), Some(("[::1]".into(), 8443)));
        assert_eq!(ok("api.example.com", Some(80)), Some(("api.example.com".into(), 80)));
        assert_eq!(ok("api.example.com", None), None);
        assert_eq!(ok("api.example.com:0", None), None);
        assert_eq!(ok("::1", Some(80)), Some(("::1".into(), 80)));
    }
}
