//! HTTP action node executor (requires `action-http` feature).
//!
//! Implements HTTP requests with method, URL interpolation, headers,
//! authentication (bearer/basic/api_key), body (json/form/raw),
//! response parsing (json/text), and status code validation.
//!
//! The URL is interpolated from workflow state, so every request, and every
//! redirect it follows, is checked against an [`HttpActionPolicy`], which by default
//! admits public addresses only. Logs and errors show the URL without its query string
//! or credentials.
//!
//! The same policy governs the notification node's webhook and the RSS node's
//! feed URL.

use std::collections::HashMap;

use adk_action::{HttpAuth, HttpBody, HttpMethod, HttpNodeConfig, interpolate_variables};
use adk_core::egress::{EgressPolicy, EgressResolver, is_non_public};
use serde_json::{Value, json};

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Which URLs the HTTP, notification, and RSS action nodes may request.
///
/// | Setting | Default |
/// |---------|---------|
/// | Schemes | `https` and `http` |
/// | Hosts | Any host |
/// | Addresses | Public only: a host that is, or resolves to, a loopback, private, link-local, cloud metadata, CGNAT, unique-local, multicast, reserved or IPv4-embedding IPv6 address is refused ([`adk_core::egress::is_non_public`]) |
/// | Redirects | Up to 5, each checked against this policy |
///
/// Host names are resolved inside the HTTP client by a resolver that refuses non-public
/// answers, so the address checked is the address connected to, and system proxies are not
/// used. [`allow_private_networks`](Self::allow_private_networks) admits every address, and
/// with it the system proxies, for workflows that call services on a private network.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::action::ActionNodeExecutor;
/// use adk_graph::action::http::HttpActionPolicy;
///
/// let policy = HttpActionPolicy::new()
///     .allow_schemes(["https"])
///     .allow_hosts(["api.example.com", "*.internal.example.com"])
///     .max_redirects(0);
/// let executor = ActionNodeExecutor::new(config).with_http_policy(policy);
/// ```
#[derive(Clone)]
pub struct HttpActionPolicy {
    allowed_schemes: Vec<String>,
    allowed_hosts: Option<Vec<String>>,
    max_redirects: usize,
    allow_private: bool,
    /// The host's secret a governed call's credential digests are keyed with.
    credential_key: Option<std::sync::Arc<[u8]>>,
}

impl std::fmt::Debug for HttpActionPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpActionPolicy")
            .field("allowed_schemes", &self.allowed_schemes)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("max_redirects", &self.max_redirects)
            .field("allow_private", &self.allow_private)
            .field("credential_key", &self.credential_key.as_ref().map(|_| "[set]"))
            .finish()
    }
}

impl Default for HttpActionPolicy {
    fn default() -> Self {
        Self {
            allowed_schemes: vec!["https".to_string(), "http".to_string()],
            allowed_hosts: None,
            max_redirects: 5,
            allow_private: false,
            credential_key: None,
        }
    }
}

impl HttpActionPolicy {
    /// Creates the default policy: `https` and `http`, any public host, 5 checked redirects.
    pub fn new() -> Self {
        Self::default()
    }

    /// Admits loopback, private, link-local and other non-public addresses, and requests
    /// through the system proxies.
    ///
    /// Use it only for workflows whose URLs an operator controls: a URL interpolated from
    /// state can then reach the host's own services and the cloud metadata endpoint.
    /// Combine it with [`allow_hosts`](Self::allow_hosts) to name the internal hosts.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_graph::action::http::HttpActionPolicy;
    ///
    /// let policy = HttpActionPolicy::new()
    ///     .allow_hosts(["billing.internal.example.com"])
    ///     .allow_private_networks();
    /// ```
    #[must_use]
    pub fn allow_private_networks(mut self) -> Self {
        self.allow_private = true;
        self
    }

    /// Replaces the permitted URL schemes, compared case-insensitively.
    #[must_use]
    pub fn allow_schemes<I, S>(mut self, schemes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_schemes =
            schemes.into_iter().map(|scheme| scheme.into().to_ascii_lowercase()).collect();
        self
    }

    /// Restricts requests to these hosts.
    ///
    /// An entry is an exact host name or IP address, or `*.example.com` for any
    /// subdomain of `example.com` (not `example.com` itself). Matching ignores case.
    #[must_use]
    pub fn allow_hosts<I, S>(mut self, hosts: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_hosts =
            Some(hosts.into_iter().map(|host| host.into().to_ascii_lowercase()).collect());
        self
    }

    /// Sets how many redirects a request may follow; `0` follows none.
    #[must_use]
    pub fn max_redirects(mut self, max_redirects: usize) -> Self {
        self.max_redirects = max_redirects;
        self
    }

    /// Keys the digests a governed call shows its credentials as with `key`, a secret only
    /// the host holds (spec 3.3 §12).
    ///
    /// Without a key a digest is a plain SHA-256 of the credential, which binds the call
    /// exactly but lets anyone who sees the decision test guesses of a weak credential,
    /// such as a password, against it. With a key the digest is an HMAC-SHA256 under it.
    /// Keep the key stable: a call's digests are part of its action-ledger key, so a new
    /// key makes an approved call, and a fork of it, a new call.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_graph::action::http::HttpActionPolicy;
    ///
    /// let policy = HttpActionPolicy::new().credential_key(std::env::var("DIGEST_KEY")?);
    /// ```
    #[must_use]
    pub fn credential_key(mut self, key: impl AsRef<[u8]>) -> Self {
        self.credential_key = Some(std::sync::Arc::from(key.as_ref()));
        self
    }

    /// The `sha256:` digest a credential named `name` is shown and bound as.
    ///
    /// Domain-separated by the name, so equal values under different names differ.
    fn digest(&self, name: &str, value: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut message = b"adk-graph.http-credential\0".to_vec();
        message.extend_from_slice(name.to_ascii_lowercase().as_bytes());
        message.push(0);
        message.extend_from_slice(value);
        let digest = match &self.credential_key {
            None => Sha256::digest(&message).to_vec(),
            Some(key) => hmac_sha256(key, &message).to_vec(),
        };
        let mut shown = String::with_capacity(7 + 64);
        shown.push_str("sha256:");
        for byte in digest {
            use std::fmt::Write as _;
            let _ = write!(shown, "{byte:02x}");
        }
        shown
    }

    /// Explains why `url` is not permitted, if it is not.
    pub(crate) fn check(&self, url: &reqwest::Url) -> std::result::Result<(), String> {
        let scheme = url.scheme();
        if !self.allowed_schemes.iter().any(|allowed| allowed == scheme) {
            return Err(format!(
                "URL scheme '{scheme}' is not permitted by the HTTP action policy; permitted \
                 schemes: {}",
                self.allowed_schemes.join(", ")
            ));
        }
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        if let Some(allowed_hosts) = &self.allowed_hosts {
            let permitted = allowed_hosts.iter().any(|allowed| match allowed.strip_prefix("*.") {
                Some(domain) => host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.')),
                None => *allowed == host,
            });
            if !permitted {
                return Err(format!(
                    "host '{host}' is not in the HTTP action policy allowlist; add it with \
                     `HttpActionPolicy::allow_hosts`"
                ));
            }
        }
        if self.allow_private {
            return Ok(());
        }
        // An address literal is never resolved, so the resolver cannot check it. IPv6 hosts
        // are serialized in brackets, and every IPv4 form as a dotted quad.
        let unbracketed = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(&host);
        let non_public = match unbracketed.parse::<std::net::IpAddr>() {
            Ok(ip) => is_non_public(ip),
            Err(_) => {
                let name = host.trim_end_matches('.');
                name == "localhost" || name.ends_with(".localhost")
            }
        };
        if non_public {
            return Err(format!(
                "host '{host}' is a loopback, private, link-local or other non-public address, \
                 which the HTTP action policy refuses by default; for a workflow whose URLs an \
                 operator controls, set `HttpActionPolicy::allow_private_networks`"
            ));
        }
        Ok(())
    }

    /// A client whose redirects are checked against this policy and, unless private networks
    /// are allowed, whose resolver refuses non-public addresses.
    pub(crate) fn client(&self) -> std::result::Result<reqwest::Client, String> {
        let mut builder = reqwest::Client::builder().redirect(self.redirect_policy());
        if !self.allow_private {
            // A proxy would resolve the host itself, out of the resolver's reach.
            let resolver = EgressResolver::new(EgressPolicy::public_only());
            builder = builder.no_proxy().dns_resolver(std::sync::Arc::new(resolver));
        }
        builder.build().map_err(|e| format!("failed to build the HTTP client: {e}"))
    }

    /// A redirect policy that re-applies [`Self::check`] to every hop.
    fn redirect_policy(&self) -> reqwest::redirect::Policy {
        if self.max_redirects == 0 {
            return reqwest::redirect::Policy::none();
        }
        let policy = self.clone();
        reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > policy.max_redirects {
                let limit = policy.max_redirects;
                attempt.error(format!("stopped after {limit} redirects"))
            } else if let Err(reason) = policy.check(attempt.url()) {
                attempt.error(format!("redirect refused: {reason}"))
            } else {
                attempt.follow()
            }
        })
    }
}

/// HMAC-SHA256 of `message` under `key` (RFC 2104).
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let pad = |byte: u8| block.iter().map(|k| k ^ byte).collect::<Vec<u8>>();
    let inner = Sha256::new().chain_update(pad(0x36)).chain_update(message).finalize();
    Sha256::new().chain_update(pad(0x5c)).chain_update(inner).finalize().into()
}

/// The URL as it may be logged: no credentials, query string, or fragment.
pub(crate) fn redact_url(url: &reqwest::Url) -> String {
    let mut shown = url.clone();
    // Both fail only for URLs that cannot carry credentials, which have none.
    let _ = shown.set_username("");
    let _ = shown.set_password(None);
    if shown.query().is_some() {
        shown.set_query(Some("redacted"));
    }
    shown.set_fragment(None);
    shown.to_string()
}

/// Execute an HTTP action node under the default [`HttpActionPolicy`].
///
/// # Errors
///
/// See [`execute_http_with_policy`].
pub async fn execute_http(config: &HttpNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    execute_http_with_policy(config, ctx, &HttpActionPolicy::default()).await
}

/// Execute an HTTP action node under `policy`.
///
/// # Example
///
/// ```rust,ignore
/// use adk_graph::action::http::{HttpActionPolicy, execute_http_with_policy};
///
/// let policy = HttpActionPolicy::new().allow_hosts(["api.example.com"]);
/// let output = execute_http_with_policy(&config, &ctx, &policy).await?;
/// ```
///
/// # Errors
///
/// Returns [`GraphError::NodeExecutionFailed`] when the interpolated URL is not
/// a valid absolute URL, the policy refuses it or a redirect, the request fails,
/// the status fails validation, or the response cannot be parsed.
pub async fn execute_http_with_policy(
    config: &HttpNodeConfig,
    ctx: &NodeContext,
    policy: &HttpActionPolicy,
) -> Result<NodeOutput> {
    let node_id = &config.standard.id;
    let fail = |message: String| GraphError::NodeExecutionFailed { node: node_id.clone(), message };
    let (client, request) = build_request(config, &ctx.state, policy).map_err(fail)?;
    tracing::debug!(
        node = %node_id,
        url = %redact_url(request.url()),
        method = ?config.method,
        "executing HTTP node"
    );
    send(config, &client, request, ctx.governed_call()).await
}

/// Fails with why the node's request cannot be built for `state`, if it cannot: an
/// invalid URL, one the policy refuses, or a client that cannot be built. Nothing is sent.
pub(crate) fn buildable(
    config: &HttpNodeConfig,
    state: &HashMap<String, Value>,
    policy: &HttpActionPolicy,
) -> Result<()> {
    build_request(config, state, policy).map(drop).map_err(|message| {
        GraphError::NodeExecutionFailed { node: config.standard.id.clone(), message }
    })
}

/// The request an HTTP node sends for `state`, with a client that checks its redirects.
///
/// The URL is checked against `policy` before anything is built. The raw URL is never
/// echoed in an error: it may carry a secret.
fn build_request(
    config: &HttpNodeConfig,
    state: &HashMap<String, Value>,
    policy: &HttpActionPolicy,
) -> std::result::Result<(reqwest::Client, reqwest::Request), String> {
    let url = reqwest::Url::parse(&interpolate_variables(&config.url, state))
        .map_err(|e| format!("HTTP node URL is not a valid absolute URL: {e}"))?;
    policy.check(&url)?;
    let client = policy.client()?;
    let mut request = match config.method {
        HttpMethod::Get => client.get(url),
        HttpMethod::Post => client.post(url),
        HttpMethod::Put => client.put(url),
        HttpMethod::Patch => client.patch(url),
        HttpMethod::Delete => client.delete(url),
        HttpMethod::Head => client.head(url),
        HttpMethod::Options => client.request(reqwest::Method::OPTIONS, url),
    };
    for (key, value) in &config.headers {
        request = request.header(key.as_str(), interpolate_variables(value, state));
    }
    request = apply_auth(request, &config.auth, state);
    request = apply_body(request, &config.body, state).map_err(|e| e.to_string())?;
    let request = request
        .build()
        .map_err(|e| format!("failed to build the HTTP request: {}", e.without_url()))?;
    Ok((client, request))
}

/// Sends `request` and maps its response to the node's output.
///
/// For a governed call (spec 3.3 §12) a failure after the request may have reached the
/// server — no answer, an answer that fails validation, or one that cannot be read — is
/// `graph.outcome_unknown`, because the call may have taken effect; only a connection that
/// was never made is a plain failure.
async fn send(
    config: &HttpNodeConfig,
    client: &reqwest::Client,
    request: reqwest::Request,
    governed: bool,
) -> Result<NodeOutput> {
    let node_id = &config.standard.id;
    let output_key = &config.standard.mapping.output_key;
    let shown_url = redact_url(request.url());
    let sent = |message: String| {
        if governed {
            crate::governed::sent_without_outcome(node_id, &message)
        } else {
            GraphError::NodeExecutionFailed { node: node_id.clone(), message }
        }
    };
    // `without_url` keeps the full URL, query string included, out of the error.
    let response = client.execute(request).await.map_err(|e| {
        let connect = e.is_connect();
        let message = format!("HTTP request to {shown_url} failed: {}", e.without_url());
        if connect {
            GraphError::NodeExecutionFailed { node: node_id.clone(), message }
        } else {
            sent(message)
        }
    })?;

    let status = response.status().as_u16();

    // Validate status code
    if let Some(pattern) = &config.response.status_validation
        && !validate_status(status, pattern)
    {
        return Err(sent(format!(
            "HTTP status {status} does not match validation pattern '{pattern}'"
        )));
    }

    // Parse response
    let result = parse_response(response, &config.response.response_type, node_id).await.map_err(
        |error| match error {
            GraphError::NodeExecutionFailed { message, .. } => sent(message),
            other => other,
        },
    )?;

    let output_value = json!({
        "status": status,
        "data": result,
    });

    Ok(NodeOutput::new().with_update(output_key, output_value))
}

/// Whether a header, query parameter or form field named `name` carries a credential.
fn is_credential(name: &str) -> bool {
    const NAMES: [&str; 8] = [
        "authorization",
        "proxy-authorization",
        "cookie",
        "set-cookie",
        "key",
        "sig",
        "code",
        "pwd",
    ];
    const PARTS: [&str; 13] = [
        "auth",
        "token",
        "secret",
        "passw",
        "cookie",
        "session",
        "signature",
        "credential",
        "api-key",
        "api_key",
        "apikey",
        "access_key",
        "private",
    ];
    let name = name.to_ascii_lowercase();
    NAMES.contains(&name.as_str()) || PARTS.iter().any(|part| name.contains(part))
}

/// Every value of the header `name`, joined as one field value.
fn header_value(
    headers: &reqwest::header::HeaderMap,
    name: &reqwest::header::HeaderName,
) -> Vec<u8> {
    let values: Vec<&[u8]> = headers.get_all(name).iter().map(|value| value.as_bytes()).collect();
    values.join(&b", "[..])
}

/// `url` with each credential in its query, and its password, replaced by its digest.
fn digested_url(policy: &HttpActionPolicy, url: &reqwest::Url) -> reqwest::Url {
    let mut shown = url.clone();
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    if pairs.iter().any(|(name, _)| is_credential(name)) {
        shown.query_pairs_mut().clear().extend_pairs(pairs.iter().map(|(name, value)| {
            let value = if is_credential(name) {
                policy.digest(name, value.as_bytes())
            } else {
                value.clone()
            };
            (name.clone(), value)
        }));
    }
    if let Some(password) = url.password() {
        // Fails only for URLs that cannot carry credentials, which have none.
        let _ = shown.set_password(Some(&policy.digest("password", password.as_bytes())));
    }
    shown
}

/// The header an API-key authentication sends its key in, lower-cased: a credential
/// whatever its name.
fn api_key_header(config: &HttpNodeConfig, state: &HashMap<String, Value>) -> Option<String> {
    match &config.auth {
        HttpAuth::ApiKey(api_key) => {
            Some(interpolate_variables(&api_key.header, state).to_ascii_lowercase())
        }
        HttpAuth::None | HttpAuth::Bearer(_) | HttpAuth::Basic(_) => None,
    }
}

/// `state` with each string under a secret-looking key replaced by its digest, and each
/// digest paired with the value it stands for.
fn digested_state(
    policy: &HttpActionPolicy,
    state: &HashMap<String, Value>,
) -> (HashMap<String, Value>, Vec<(String, String)>) {
    let mut shown = state.clone();
    let mut digests = Vec::new();
    for (key, value) in state {
        if let Value::String(raw) = value
            && is_credential(key)
        {
            let digest = policy.digest(key, raw.as_bytes());
            shown.insert(key.clone(), Value::String(digest.clone()));
            digests.push((digest, raw.clone()));
        }
    }
    (shown, digests)
}

/// `body` with each string under a secret-looking key, at any depth, replaced by its digest.
fn digested_json(policy: &HttpActionPolicy, body: &Value, key: Option<&str>) -> Value {
    match body {
        Value::String(raw) if key.is_some_and(is_credential) => {
            Value::String(policy.digest(key.unwrap_or_default(), raw.as_bytes()))
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(name, value)| (name.clone(), digested_json(policy, value, Some(name))))
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| digested_json(policy, item, key)).collect())
        }
        other => other.clone(),
    }
}

/// `approved` with each digest under a secret-looking key replaced by the value `current`
/// holds at the same place; fails naming the field whose credential changed.
fn restored_json(
    policy: &HttpActionPolicy,
    approved: &Value,
    current: Option<&Value>,
    key: Option<&str>,
) -> std::result::Result<Value, String> {
    match approved {
        Value::String(digest)
            if key.is_some_and(is_credential) && digest.starts_with("sha256:") =>
        {
            let name = key.unwrap_or_default();
            current
                .and_then(Value::as_str)
                .filter(|raw| policy.digest(name, raw.as_bytes()) == *digest)
                .map(|raw| Value::String(raw.to_string()))
                .ok_or_else(|| format!("the credential in body field '{name}'"))
        }
        Value::Object(map) => map
            .iter()
            .map(|(name, value)| {
                restored_json(
                    policy,
                    value,
                    current.and_then(|current| current.get(name)),
                    Some(name),
                )
                .map(|value| (name.clone(), value))
            })
            .collect::<std::result::Result<serde_json::Map<_, _>, _>>()
            .map(Value::Object),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(at, item)| {
                restored_json(policy, item, current.and_then(|current| current.get(at)), key)
            })
            .collect::<std::result::Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other.clone()),
    }
}

/// Whether `text` still holds a credential digest: `sha256:` and 64 hex digits.
fn holds_digest(text: &str) -> bool {
    text.match_indices("sha256:").any(|(at, _)| {
        let hex = &text.as_bytes()[at + 7..];
        hex.len() >= 64 && hex[..64].iter().all(u8::is_ascii_hexdigit)
    })
}

/// The call an HTTP node makes for `state`, as an approver sees it and as its record
/// binds it (spec 3.3 §12, §7.1).
///
/// Carries the method, the full URL with its query, every header (authentication
/// included) and the body. Credentials appear as `sha256:` digests, so the call is bound
/// exactly while the decision shows no secret: authorization and cookie headers, an API
/// key's header whatever its name, headers, query parameters, form fields and JSON body
/// fields with secret-looking names, and in a raw body the values of state keys with
/// secret-looking names. A request that cannot be built is described by its method and
/// why, never by the state it read.
pub(crate) fn call_args(
    config: &HttpNodeConfig,
    state: &HashMap<String, Value>,
    policy: &HttpActionPolicy,
) -> Option<Value> {
    let request = match build_request(config, state, policy) {
        Ok((_, request)) => request,
        Err(error) => {
            return Some(json!({ "method": format!("{:?}", config.method), "error": error }));
        }
    };
    let api_key = api_key_header(config, state);
    let secret = |name: &str| is_credential(name) || api_key.as_deref() == Some(name);
    let mut headers = serde_json::Map::new();
    for name in request.headers().keys() {
        let raw = header_value(request.headers(), name);
        let value = if secret(name.as_str()) {
            policy.digest(name.as_str(), &raw)
        } else {
            String::from_utf8_lossy(&raw).into_owned()
        };
        headers.insert(name.as_str().to_string(), Value::String(value));
    }
    let mut call = json!({
        "method": request.method().as_str(),
        "url": digested_url(policy, request.url()).as_str(),
        "headers": headers,
    });
    match &config.body {
        HttpBody::None => {}
        HttpBody::Json { data } => {
            call["body"] = digested_json(policy, &interpolate_json_values(data, state), None);
        }
        HttpBody::Form { fields } => {
            let form: serde_json::Map<String, Value> = fields
                .iter()
                .map(|(name, value)| {
                    let value = interpolate_variables(value, state);
                    let value = if is_credential(name) {
                        policy.digest(name, value.as_bytes())
                    } else {
                        value
                    };
                    (name.clone(), Value::String(value))
                })
                .collect();
            call["form"] = Value::Object(form);
        }
        HttpBody::Raw { content, .. } => {
            let (shown, _) = digested_state(policy, state);
            call["rawBody"] = Value::String(interpolate_variables(content, &shown));
        }
    }
    Some(call)
}

/// Sends exactly `args`, a call [`call_args`] described and an approver may have edited
/// (spec 3.3 §7.1, §7.5).
///
/// Each digest in `args` is replaced by the credential `state` holds now; a digest that
/// no longer matches it refuses the call, which is then not sent.
///
/// # Errors
///
/// Returns [`GovernedError::Refused`](crate::governed::GovernedError::Refused) when a
/// credential changed or the policy refuses the URL, and the errors
/// [`execute_http_with_policy`] returns.
pub(crate) async fn run_call(
    config: &HttpNodeConfig,
    state: &HashMap<String, Value>,
    policy: &HttpActionPolicy,
    args: &Value,
    governed: bool,
) -> Result<NodeOutput> {
    let node = config.standard.id.clone();
    let refuse = |reason: String| -> GraphError {
        crate::governed::GovernedError::Refused { node: node.clone(), reason }.into()
    };
    let changed = |what: String| {
        refuse(format!("{what} changed since the call was approved; the call was not sent"))
    };
    let (client, current) = build_request(config, state, policy).map_err(&refuse)?;
    let api_key = api_key_header(config, state);
    let secret = |name: &str| {
        is_credential(name) || api_key.as_deref() == Some(name.to_ascii_lowercase().as_str())
    };

    let method = args["method"]
        .as_str()
        .and_then(|method| reqwest::Method::from_bytes(method.as_bytes()).ok())
        .ok_or_else(|| refuse("the call has no valid method".to_string()))?;
    let mut url = args["url"]
        .as_str()
        .and_then(|url| reqwest::Url::parse(url).ok())
        .ok_or_else(|| refuse("the call has no valid URL".to_string()))?;

    // Credentials in the query and the password come back from the current request.
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    if pairs.iter().any(|(name, value)| is_credential(name) && value.starts_with("sha256:")) {
        let mut resolved = Vec::with_capacity(pairs.len());
        for (name, value) in pairs {
            if is_credential(&name) && value.starts_with("sha256:") {
                let current_value = current
                    .url()
                    .query_pairs()
                    .find(|(key, raw)| {
                        *key == name && policy.digest(&name, raw.as_bytes()) == value
                    })
                    .map(|(_, raw)| raw.into_owned())
                    .ok_or_else(|| {
                        changed(format!("the credential in query parameter '{name}'"))
                    })?;
                resolved.push((name, current_value));
            } else {
                resolved.push((name, value));
            }
        }
        url.query_pairs_mut().clear().extend_pairs(resolved);
    }
    if url.password().is_some() {
        if digested_url(policy, current.url()).password() != url.password() {
            return Err(changed("the URL's password".to_string()));
        }
        let _ = url.set_password(current.url().password());
    }
    policy.check(&url).map_err(&refuse)?;

    let mut request = client.request(method, url);
    if let Some(headers) = args["headers"].as_object() {
        for (name, value) in headers {
            let value = value.as_str().unwrap_or_default();
            if secret(name) && value.starts_with("sha256:") {
                let header = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| refuse(format!("'{name}' is not a valid header name")))?;
                let raw = header_value(current.headers(), &header);
                if raw.is_empty() || policy.digest(name, &raw) != value {
                    return Err(changed(format!("the credential in header '{name}'")));
                }
                request = request.header(header, raw);
            } else {
                request = request.header(name.as_str(), value);
            }
        }
    }
    if let Some(body) = args.get("body") {
        let current_body = match &config.body {
            HttpBody::Json { data } => Some(interpolate_json_values(data, state)),
            _ => None,
        };
        let body = restored_json(policy, body, current_body.as_ref(), None).map_err(changed)?;
        request = request.json(&body);
    } else if let Some(form) = args["form"].as_object() {
        let current_form: HashMap<String, String> = match &config.body {
            HttpBody::Form { fields } => fields
                .iter()
                .map(|(name, value)| (name.clone(), interpolate_variables(value, state)))
                .collect(),
            _ => HashMap::new(),
        };
        let mut fields = HashMap::with_capacity(form.len());
        for (name, value) in form {
            let value = value.as_str().unwrap_or_default().to_string();
            let value = if is_credential(name) && value.starts_with("sha256:") {
                current_form
                    .get(name)
                    .filter(|raw| policy.digest(name, raw.as_bytes()) == value)
                    .cloned()
                    .ok_or_else(|| changed(format!("the credential in form field '{name}'")))?
            } else {
                value
            };
            fields.insert(name.clone(), value);
        }
        request = request.form(&fields);
    } else if let Some(raw) = args["rawBody"].as_str() {
        let (_, digests) = digested_state(policy, state);
        let mut raw = raw.to_string();
        for (digest, value) in &digests {
            raw = raw.replace(digest.as_str(), value);
        }
        if holds_digest(&raw) {
            return Err(changed("a credential in the raw body".to_string()));
        }
        request = request.body(raw);
    }
    let request = request
        .build()
        .map_err(|e| refuse(format!("the call could not be built: {}", e.without_url())))?;
    tracing::debug!(node = %config.standard.id, url = %redact_url(request.url()), "sending an approved HTTP call");
    send(config, &client, request, governed).await
}

/// Apply authentication to the request builder.
fn apply_auth(
    request: reqwest::RequestBuilder,
    auth: &HttpAuth,
    state: &HashMap<String, Value>,
) -> reqwest::RequestBuilder {
    match auth {
        HttpAuth::None => request,
        HttpAuth::Bearer(bearer) => {
            let token = interpolate_variables(&bearer.token, state);
            request.bearer_auth(token)
        }
        HttpAuth::Basic(basic) => {
            let username = interpolate_variables(&basic.username, state);
            let password = interpolate_variables(&basic.password, state);
            request.basic_auth(username, Some(password))
        }
        HttpAuth::ApiKey(api_key) => {
            let header = interpolate_variables(&api_key.header, state);
            let value = interpolate_variables(&api_key.value, state);
            request.header(header, value)
        }
    }
}

/// Apply body to the request builder.
fn apply_body(
    request: reqwest::RequestBuilder,
    body: &HttpBody,
    state: &HashMap<String, Value>,
) -> Result<reqwest::RequestBuilder> {
    match body {
        HttpBody::None => Ok(request),
        HttpBody::Json { data } => {
            // Interpolate string values within the JSON data
            let interpolated = interpolate_json_values(data, state);
            Ok(request.json(&interpolated))
        }
        HttpBody::Form { fields } => {
            let interpolated: HashMap<String, String> =
                fields.iter().map(|(k, v)| (k.clone(), interpolate_variables(v, state))).collect();
            Ok(request.form(&interpolated))
        }
        HttpBody::Raw { content, content_type } => {
            let interpolated_content = interpolate_variables(content, state);
            let interpolated_ct = interpolate_variables(content_type, state);
            Ok(request.header("Content-Type", interpolated_ct).body(interpolated_content))
        }
    }
}

/// Recursively interpolate string values within a JSON value.
fn interpolate_json_values(value: &Value, state: &HashMap<String, Value>) -> Value {
    match value {
        Value::String(s) => {
            let interpolated = interpolate_variables(s, state);
            Value::String(interpolated)
        }
        Value::Object(map) => {
            let new_map: serde_json::Map<String, Value> =
                map.iter().map(|(k, v)| (k.clone(), interpolate_json_values(v, state))).collect();
            Value::Object(new_map)
        }
        Value::Array(arr) => {
            let new_arr: Vec<Value> =
                arr.iter().map(|v| interpolate_json_values(v, state)).collect();
            Value::Array(new_arr)
        }
        other => other.clone(),
    }
}

/// Parse the HTTP response based on the configured response type.
async fn parse_response(
    response: reqwest::Response,
    response_type: &str,
    node_id: &str,
) -> Result<Value> {
    match response_type {
        "json" => {
            let json_value: Value =
                response.json().await.map_err(|e| GraphError::NodeExecutionFailed {
                    node: node_id.to_string(),
                    message: format!("failed to parse JSON response: {e}"),
                })?;
            Ok(json_value)
        }
        _ => {
            // Default to text
            let text = response.text().await.map_err(|e| GraphError::NodeExecutionFailed {
                node: node_id.to_string(),
                message: format!("failed to read response text: {e}"),
            })?;
            Ok(Value::String(text))
        }
    }
}

/// Validate an HTTP status code against a pattern string.
///
/// Supported patterns:
/// - Single code: `"200"`
/// - Comma-separated: `"200,201,204"`
/// - Range: `"200-299"`
/// - Mixed: `"200-299,404"`
fn validate_status(status: u16, pattern: &str) -> bool {
    for part in pattern.split(',') {
        let part = part.trim();
        if let Some((start_str, end_str)) = part.split_once('-') {
            if let (Ok(start), Ok(end)) =
                (start_str.trim().parse::<u16>(), end_str.trim().parse::<u16>())
                && status >= start
                && status <= end
            {
                return true;
            }
        } else if let Ok(code) = part.parse::<u16>()
            && status == code
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> reqwest::Url {
        reqwest::Url::parse(raw).expect("valid URL")
    }

    #[test]
    fn a_file_url_is_rejected_by_default() {
        let error = HttpActionPolicy::default().check(&url("file:///etc/passwd")).unwrap_err();
        assert!(error.contains("scheme 'file' is not permitted"), "{error}");
    }

    #[test]
    fn http_and_https_to_public_hosts_are_permitted_by_default() {
        let policy = HttpActionPolicy::default();
        for raw in ["https://example.com/a", "http://93.184.216.34/", "https://[2606:4700::1111]/"]
        {
            assert_eq!(policy.check(&url(raw)), Ok(()), "{raw}");
        }
    }

    #[test]
    fn non_public_addresses_are_refused_unless_private_networks_are_allowed() {
        let internal = [
            "http://127.0.0.1:8080/",
            "http://0x7f.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1/",
            "http://172.16.4.2/",
            "http://192.168.1.1/",
            "http://100.64.0.1/",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[::ffff:169.254.169.254]/",
            "http://[64:ff9b::a9fe:a9fe]/",
            "http://[fd00:ec2::254]/",
            "http://[fe80::1]/",
            "http://localhost/",
            "http://LOCALHOST./",
            "http://api.localhost/",
        ];
        let default = HttpActionPolicy::default();
        let opted_in = HttpActionPolicy::new().allow_private_networks();
        for raw in internal {
            let error = default.check(&url(raw)).unwrap_err();
            assert!(error.contains("allow_private_networks"), "{raw}: {error}");
            assert_eq!(opted_in.check(&url(raw)), Ok(()), "{raw}");
        }
    }

    #[test]
    fn an_allowlisted_host_is_still_refused_at_a_non_public_address() {
        let policy = HttpActionPolicy::new().allow_hosts(["127.0.0.1"]);
        assert!(policy.check(&url("http://127.0.0.1/")).unwrap_err().contains("non-public"));
        let opted_in = policy.allow_private_networks();
        assert_eq!(opted_in.check(&url("http://127.0.0.1/")), Ok(()));
    }

    /// The client checks the address a name resolves to when it connects, so a name that
    /// resolves to loopback is refused on the first request and on every redirect.
    #[tokio::test]
    async fn the_default_client_refuses_a_name_that_resolves_to_loopback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = format!("http://localhost:{port}/");

        let error = HttpActionPolicy::default().client().unwrap().get(&target).send().await;
        let error = error.expect_err("a name for loopback must not be connected to");
        assert!(error.is_connect(), "{error:?}");
        let accepted =
            tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
        assert!(accepted.is_err(), "no connection reached the listener");

        let opted_in = HttpActionPolicy::new().allow_private_networks().client().unwrap();
        let request = tokio::spawn(async move { opted_in.get(&target).send().await });
        let accepted =
            tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await;
        assert!(matches!(accepted, Ok(Ok(_))), "the opt-in connects");
        request.abort();
    }

    #[test]
    fn the_host_allowlist_matches_exact_hosts_and_subdomain_wildcards() {
        let policy = HttpActionPolicy::new().allow_hosts(["api.example.com", "*.Internal.test"]);
        let outcomes: Vec<bool> = [
            "https://API.example.com/v1",
            "https://a.internal.test/",
            "https://b.a.internal.test/",
            "https://internal.test/",
            "https://evilinternal.test/",
            "https://example.com/",
        ]
        .into_iter()
        .map(|raw| policy.check(&url(raw)).is_ok())
        .collect();
        assert_eq!(outcomes, vec![true, true, true, false, false, false]);
    }

    #[test]
    fn a_scheme_list_replaces_the_default() {
        let policy = HttpActionPolicy::new().allow_schemes(["HTTPS"]);
        assert!(policy.check(&url("http://example.com/")).is_err());
        assert!(policy.check(&url("https://example.com/")).is_ok());
    }

    #[test]
    fn a_logged_url_has_no_query_or_credentials() {
        assert_eq!(
            redact_url(&url("https://user:pass@example.com/path?token=secret#frag")),
            "https://example.com/path?redacted"
        );
        assert_eq!(redact_url(&url("https://example.com/path")), "https://example.com/path");
    }

    #[test]
    fn hmac_sha256_matches_rfc_4231() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        let hex: String = mac.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843");
    }

    #[test]
    fn a_keyed_digest_cannot_be_checked_without_the_key() {
        let plain = HttpActionPolicy::new();
        let keyed = HttpActionPolicy::new().credential_key("host-secret");
        let guess = plain.digest("password", b"Summer2026!");
        assert_ne!(keyed.digest("password", b"Summer2026!"), guess);
        assert_eq!(
            keyed.digest("password", b"Summer2026!"),
            keyed.digest("Password", b"Summer2026!"),
            "the name is compared without case"
        );
        assert!(!format!("{keyed:?}").contains("host-secret"));
    }

    #[test]
    fn test_validate_status_single() {
        assert!(validate_status(200, "200"));
        assert!(!validate_status(201, "200"));
    }

    #[test]
    fn test_validate_status_range() {
        assert!(validate_status(200, "200-299"));
        assert!(validate_status(250, "200-299"));
        assert!(validate_status(299, "200-299"));
        assert!(!validate_status(300, "200-299"));
        assert!(!validate_status(199, "200-299"));
    }

    #[test]
    fn test_validate_status_comma_separated() {
        assert!(validate_status(200, "200,201,204"));
        assert!(validate_status(201, "200,201,204"));
        assert!(validate_status(204, "200,201,204"));
        assert!(!validate_status(202, "200,201,204"));
    }

    #[test]
    fn test_validate_status_mixed() {
        assert!(validate_status(200, "200-299,404"));
        assert!(validate_status(404, "200-299,404"));
        assert!(!validate_status(500, "200-299,404"));
    }
}
