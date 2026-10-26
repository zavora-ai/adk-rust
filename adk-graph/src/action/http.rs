//! HTTP action node executor (requires `action-http` feature).
//!
//! Implements HTTP requests with method, URL interpolation, headers,
//! authentication (bearer/basic/api_key), body (json/form/raw),
//! response parsing (json/text), and status code validation.
//!
//! The URL is interpolated from workflow state, so every request, and every
//! redirect it follows, is checked against an [`HttpActionPolicy`]. Logs and
//! errors show the URL without its query string or credentials.
//!
//! The same policy governs the notification node's webhook and the RSS node's
//! feed URL.

use std::collections::HashMap;

use adk_action::{HttpAuth, HttpBody, HttpMethod, HttpNodeConfig, interpolate_variables};
use serde_json::{Value, json};

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Which URLs the HTTP, notification, and RSS action nodes may request.
///
/// | Setting | Default |
/// |---------|---------|
/// | Schemes | `https` and `http` |
/// | Hosts | Any host |
/// | Redirects | Up to 5, each checked against this policy |
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
#[derive(Debug, Clone)]
pub struct HttpActionPolicy {
    allowed_schemes: Vec<String>,
    allowed_hosts: Option<Vec<String>>,
    max_redirects: usize,
}

impl Default for HttpActionPolicy {
    fn default() -> Self {
        Self {
            allowed_schemes: vec!["https".to_string(), "http".to_string()],
            allowed_hosts: None,
            max_redirects: 5,
        }
    }
}

impl HttpActionPolicy {
    /// Creates the default policy: `https` and `http`, any host, 5 checked redirects.
    pub fn new() -> Self {
        Self::default()
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
        let Some(allowed_hosts) = &self.allowed_hosts else { return Ok(()) };
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let permitted = allowed_hosts.iter().any(|allowed| match allowed.strip_prefix("*.") {
            Some(domain) => host.strip_suffix(domain).is_some_and(|rest| rest.ends_with('.')),
            None => *allowed == host,
        });
        if permitted {
            Ok(())
        } else {
            Err(format!(
                "host '{host}' is not in the HTTP action policy allowlist; add it with \
                 `HttpActionPolicy::allow_hosts`"
            ))
        }
    }

    /// A client whose redirects are checked against this policy.
    pub(crate) fn client(&self) -> std::result::Result<reqwest::Client, String> {
        reqwest::Client::builder()
            .redirect(self.redirect_policy())
            .build()
            .map_err(|e| format!("failed to build the HTTP client: {e}"))
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
    send(config, &client, request).await
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
async fn send(
    config: &HttpNodeConfig,
    client: &reqwest::Client,
    request: reqwest::Request,
) -> Result<NodeOutput> {
    let node_id = &config.standard.id;
    let output_key = &config.standard.mapping.output_key;
    let shown_url = redact_url(request.url());
    // `without_url` keeps the full URL, query string included, out of the error.
    let response = client.execute(request).await.map_err(|e| GraphError::NodeExecutionFailed {
        node: node_id.clone(),
        message: format!("HTTP request to {shown_url} failed: {}", e.without_url()),
    })?;

    let status = response.status().as_u16();

    // Validate status code
    if let Some(pattern) = &config.response.status_validation
        && !validate_status(status, pattern)
    {
        return Err(GraphError::NodeExecutionFailed {
            node: node_id.clone(),
            message: format!("HTTP status {status} does not match validation pattern '{pattern}'"),
        });
    }

    // Parse response
    let result = parse_response(response, &config.response.response_type, node_id).await?;

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

/// The `sha256:` digest a credential named `name` is shown and bound as.
///
/// Domain-separated by the name, so equal values under different names differ.
fn credential_digest(name: &str, value: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"adk-graph.http-credential\0");
    hasher.update(name.to_ascii_lowercase().as_bytes());
    hasher.update(b"\0");
    hasher.update(value);
    format!("sha256:{:x}", hasher.finalize())
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
fn digested_url(url: &reqwest::Url) -> reqwest::Url {
    let mut shown = url.clone();
    let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    if pairs.iter().any(|(name, _)| is_credential(name)) {
        shown.query_pairs_mut().clear().extend_pairs(pairs.iter().map(|(name, value)| {
            let value = if is_credential(name) {
                credential_digest(name, value.as_bytes())
            } else {
                value.clone()
            };
            (name.clone(), value)
        }));
    }
    if let Some(password) = url.password() {
        // Fails only for URLs that cannot carry credentials, which have none.
        let _ = shown.set_password(Some(&credential_digest("password", password.as_bytes())));
    }
    shown
}

/// The call an HTTP node makes for `state`, as an approver sees it and as its record
/// binds it (spec 3.3 §12, §7.1).
///
/// Carries the method, the full URL with its query, every header (authentication
/// included) and the body. Credentials — authorization and cookie headers, and headers,
/// query parameters and form fields with secret-looking names — appear as `sha256:`
/// digests, so the call is bound exactly while the decision shows no secret.
pub(crate) fn call_args(
    config: &HttpNodeConfig,
    state: &HashMap<String, Value>,
    policy: &HttpActionPolicy,
) -> Option<Value> {
    let (_, request) = build_request(config, state, policy).ok()?;
    let mut headers = serde_json::Map::new();
    for name in request.headers().keys() {
        let raw = header_value(request.headers(), name);
        let value = if is_credential(name.as_str()) {
            credential_digest(name.as_str(), &raw)
        } else {
            String::from_utf8_lossy(&raw).into_owned()
        };
        headers.insert(name.as_str().to_string(), Value::String(value));
    }
    let mut call = json!({
        "method": request.method().as_str(),
        "url": digested_url(request.url()).as_str(),
        "headers": headers,
    });
    match &config.body {
        HttpBody::None => {}
        HttpBody::Json { data } => call["body"] = interpolate_json_values(data, state),
        HttpBody::Form { fields } => {
            let form: serde_json::Map<String, Value> = fields
                .iter()
                .map(|(name, value)| {
                    let value = interpolate_variables(value, state);
                    let value = if is_credential(name) {
                        credential_digest(name, value.as_bytes())
                    } else {
                        value
                    };
                    (name.clone(), Value::String(value))
                })
                .collect();
            call["form"] = Value::Object(form);
        }
        HttpBody::Raw { content, .. } => {
            call["rawBody"] = Value::String(interpolate_variables(content, state));
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
) -> Result<NodeOutput> {
    let node = config.standard.id.clone();
    let refuse = |reason: String| -> GraphError {
        crate::governed::GovernedError::Refused { node: node.clone(), reason }.into()
    };
    let changed = |what: String| {
        refuse(format!("{what} changed since the call was approved; the call was not sent"))
    };
    let (client, current) = build_request(config, state, policy).map_err(&refuse)?;

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
                        *key == name && credential_digest(&name, raw.as_bytes()) == value
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
        if digested_url(current.url()).password() != url.password() {
            return Err(changed("the URL's password".to_string()));
        }
        let _ = url.set_password(current.url().password());
    }
    policy.check(&url).map_err(&refuse)?;

    let mut request = client.request(method, url);
    if let Some(headers) = args["headers"].as_object() {
        for (name, value) in headers {
            let value = value.as_str().unwrap_or_default();
            if is_credential(name) && value.starts_with("sha256:") {
                let header = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| refuse(format!("'{name}' is not a valid header name")))?;
                let raw = header_value(current.headers(), &header);
                if raw.is_empty() || credential_digest(name, &raw) != value {
                    return Err(changed(format!("the credential in header '{name}'")));
                }
                request = request.header(header, raw);
            } else {
                request = request.header(name.as_str(), value);
            }
        }
    }
    if let Some(body) = args.get("body") {
        request = request.json(body);
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
                    .filter(|raw| credential_digest(name, raw.as_bytes()) == value)
                    .cloned()
                    .ok_or_else(|| changed(format!("the credential in form field '{name}'")))?
            } else {
                value
            };
            fields.insert(name.clone(), value);
        }
        request = request.form(&fields);
    } else if let Some(raw) = args["rawBody"].as_str() {
        request = request.body(raw.to_string());
    }
    let request = request
        .build()
        .map_err(|e| refuse(format!("the call could not be built: {}", e.without_url())))?;
    tracing::debug!(node = %config.standard.id, url = %redact_url(request.url()), "sending an approved HTTP call");
    send(config, &client, request).await
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
    fn http_and_https_to_any_host_are_permitted_by_default() {
        let policy = HttpActionPolicy::default();
        assert_eq!(
            (policy.check(&url("https://example.com/a")), policy.check(&url("http://10.0.0.1/"))),
            (Ok(()), Ok(()))
        );
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
