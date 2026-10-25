//! Signs outgoing requests with Web Bot Auth. Spec 3.5 §7.3 "Outbound".

use std::sync::Arc;
use std::time::Duration;

use adk_core::{Clock, SystemClock};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http::header::HeaderValue;
use http::{HeaderMap, Method, Uri};

use super::keys::Ed25519SigningKey;
use super::message::{ComponentId, RequestHead, SignatureInput};
use super::structured::{BareItem, Dictionary, Item, ListEntry};
use super::{
    SIGNATURE, SIGNATURE_AGENT, SIGNATURE_INPUT, WEB_BOT_AUTH_TAG, WebBotAuthError, combined_field,
};

/// The time window and nonce of one signature.
///
/// [`HttpSignatureSigner`] derives them from its clock, lifetime and a random nonce;
/// [`sign_head_with`](HttpSignatureSigner::sign_head_with) takes them explicitly, for test
/// vectors and callers that issue their own nonces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningParams {
    /// `created`, Unix seconds.
    pub created: i64,
    /// `expires`, Unix seconds.
    pub expires: i64,
    /// `nonce`, when one is sent.
    pub nonce: Option<String>,
}

/// Signs outgoing requests as a Web Bot Auth agent (draft-ietf-webbotauth-httpsig-protocol-00
/// §5.2), so a site's crawls, `web_fetch` calls and verifier requests identify it.
/// Spec 3.5 §7.3 "Outbound".
///
/// Each signature covers `@authority`, `@method`, `@path` and `@query` by default (the draft's
/// required `@authority` narrowed to one request, §5.2) and the signer's `Signature-Agent`
/// member, and carries `created`, `keyid` (the key's JWK thumbprint), `alg="ed25519"`,
/// `expires` (five minutes later by default), a random `nonce` and `tag="web-bot-auth"`, in
/// that order. The `Signature-Agent` value is the origin whose
/// `/.well-known/http-message-signatures-directory` publishes the key
/// ([`KeyDirectory`](super::KeyDirectory)).
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_auth::web_bot_auth::{Ed25519SigningKey, HttpSignatureSigner};
///
/// let key = Arc::new(Ed25519SigningKey::generate().unwrap());
/// let signer = HttpSignatureSigner::builder(key, "https://crawler.example").build().unwrap();
///
/// let mut request = reqwest::Client::new().get("https://news.example/feed").build().unwrap();
/// signer.sign_reqwest(&mut request).unwrap();
/// assert_eq!(request.headers()["signature-agent"], "sig1=\"https://crawler.example\"");
/// assert!(request.headers()["signature-input"].to_str().unwrap().contains("tag=\"web-bot-auth\""));
/// ```
pub struct HttpSignatureSigner {
    key: Arc<Ed25519SigningKey>,
    signature_agent: String,
    label: String,
    agent_member: String,
    components: Vec<ComponentId>,
    lifetime: Duration,
    nonce: bool,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for HttpSignatureSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSignatureSigner")
            .field("key", &self.key)
            .field("signature_agent", &self.signature_agent)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl HttpSignatureSigner {
    /// Starts a builder for `key`, publishing its directory at the `signature_agent` origin.
    pub fn builder(
        key: Arc<Ed25519SigningKey>,
        signature_agent: impl Into<String>,
    ) -> HttpSignatureSignerBuilder {
        HttpSignatureSignerBuilder {
            key,
            signature_agent: signature_agent.into(),
            label: "sig1".to_string(),
            agent_member: None,
            components: vec![
                ComponentId::new("@authority"),
                ComponentId::new("@method"),
                ComponentId::new("@path"),
                ComponentId::new("@query"),
            ],
            lifetime: Duration::from_secs(300),
            nonce: true,
            clock: Arc::new(SystemClock),
        }
    }

    /// The signing key's thumbprint, sent as `keyid`.
    pub fn keyid(&self) -> &str {
        self.key.thumbprint()
    }

    /// Signs an [`http::Request`].
    ///
    /// # Errors
    ///
    /// As [`sign_head`](Self::sign_head).
    pub fn sign_request<B>(&self, request: &mut http::Request<B>) -> Result<(), WebBotAuthError> {
        let method = request.method().clone();
        let uri = request.uri().clone();
        self.sign_head(&method, &uri, request.headers_mut())
    }

    /// Signs a [`reqwest::Request`] before it is sent.
    ///
    /// # Errors
    ///
    /// As [`sign_head`](Self::sign_head).
    pub fn sign_reqwest(&self, request: &mut reqwest::Request) -> Result<(), WebBotAuthError> {
        let method = request.method().clone();
        let uri: Uri = request.url().as_str().parse().map_err(|e| {
            WebBotAuthError::SignatureBase { reason: format!("the request URL is not a URI: {e}") }
        })?;
        self.sign_head(&method, &uri, request.headers_mut())
    }

    /// Signs a request given its method, absolute URI and headers, adding the
    /// `Signature-Agent`, `Signature-Input` and `Signature` fields.
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::SignatureBase`] when a covered component is absent from the
    /// request, and [`WebBotAuthError::MalformedField`] when an existing signature field cannot
    /// be extended.
    pub fn sign_head(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &mut HeaderMap,
    ) -> Result<(), WebBotAuthError> {
        let created = self.clock.now().timestamp();
        let lifetime = i64::try_from(self.lifetime.as_secs()).unwrap_or(i64::MAX);
        let nonce = self.nonce.then(|| {
            let mut bytes = [0u8; 32];
            // A failed fill leaves a zero nonce; the signature stays valid, only less unique.
            let _ = aws_lc_rs::rand::fill(&mut bytes);
            STANDARD.encode(bytes)
        });
        self.sign_head_with(
            method,
            uri,
            headers,
            SigningParams { created, expires: created.saturating_add(lifetime), nonce },
        )
    }

    /// Signs a request with an explicit time window and nonce.
    ///
    /// # Errors
    ///
    /// As [`sign_head`](Self::sign_head).
    pub fn sign_head_with(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &mut HeaderMap,
        params: SigningParams,
    ) -> Result<(), WebBotAuthError> {
        let malformed = |field: &str, reason: String| WebBotAuthError::MalformedField {
            field: field.to_string(),
            reason,
        };
        let field = |headers: &HeaderMap, name: &str| -> Result<String, WebBotAuthError> {
            Ok(combined_field(headers, name)?.unwrap_or_default())
        };
        let mut agents = Dictionary::parse(&field(headers, SIGNATURE_AGENT)?)
            .map_err(|e| malformed(SIGNATURE_AGENT, e.to_string()))?;
        agents.insert(
            self.agent_member.clone(),
            ListEntry::Item(Item::new(BareItem::String(self.signature_agent.clone()))),
        );
        set_field(headers, SIGNATURE_AGENT, &agents)?;

        let mut components = self.components.clone();
        components.push(ComponentId::signature_agent(self.agent_member.clone()));
        let mut input = SignatureInput::new(components);
        input.params.insert("created", BareItem::Integer(params.created));
        input.params.insert("keyid", BareItem::String(self.key.thumbprint().to_string()));
        input.params.insert("alg", BareItem::String("ed25519".into()));
        input.params.insert("expires", BareItem::Integer(params.expires));
        if let Some(nonce) = params.nonce {
            input.params.insert("nonce", BareItem::String(nonce));
        }
        input.params.insert("tag", BareItem::String(WEB_BOT_AUTH_TAG.into()));

        let base = RequestHead::new(method, uri, headers).signature_base(&input)?;
        let signature = self.key.sign(base.as_bytes());

        let mut inputs = Dictionary::parse(&field(headers, SIGNATURE_INPUT)?)
            .map_err(|e| malformed(SIGNATURE_INPUT, e.to_string()))?;
        let mut signatures = Dictionary::parse(&field(headers, SIGNATURE)?)
            .map_err(|e| malformed(SIGNATURE, e.to_string()))?;
        inputs.insert(self.label.clone(), ListEntry::InnerList(input.to_inner_list()));
        signatures.insert(
            self.label.clone(),
            ListEntry::Item(Item::new(BareItem::ByteSequence(signature.to_vec()))),
        );
        set_field(headers, SIGNATURE_INPUT, &inputs)?;
        set_field(headers, SIGNATURE, &signatures)
    }
}

fn set_field(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &Dictionary,
) -> Result<(), WebBotAuthError> {
    let invalid =
        |reason: String| WebBotAuthError::MalformedField { field: name.to_string(), reason };
    let serialized = value.serialize().map_err(|e| invalid(e.to_string()))?;
    let value = HeaderValue::from_str(&serialized).map_err(|e| invalid(e.to_string()))?;
    headers.insert(name, value);
    Ok(())
}

/// Builds an [`HttpSignatureSigner`].
pub struct HttpSignatureSignerBuilder {
    key: Arc<Ed25519SigningKey>,
    signature_agent: String,
    label: String,
    agent_member: Option<String>,
    components: Vec<ComponentId>,
    lifetime: Duration,
    nonce: bool,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for HttpSignatureSignerBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSignatureSignerBuilder")
            .field("signature_agent", &self.signature_agent)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl HttpSignatureSignerBuilder {
    /// Sets the signature label. Default `sig1`.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// Sets the `Signature-Agent` member key. Default: the label.
    pub fn agent_member(mut self, member: impl Into<String>) -> Self {
        self.agent_member = Some(member.into());
        self
    }

    /// Replaces the covered components; the signer's own `Signature-Agent` member is always
    /// added. Include `@authority` or `@target-uri`, as the draft requires (§5.2). An outer
    /// signer covering another signature also covers its `signature-input`, `signature` and
    /// `signature-agent` members (draft §5.2.2).
    pub fn components(mut self, components: Vec<ComponentId>) -> Self {
        self.components = components;
        self
    }

    /// Sets how long each signature is valid. Default 5 minutes; the draft recommends at most
    /// 24 hours (§5.2).
    pub fn lifetime(mut self, lifetime: Duration) -> Self {
        self.lifetime = lifetime;
        self
    }

    /// Sends a random `nonce` with each signature. Default on.
    pub fn nonce(mut self, nonce: bool) -> Self {
        self.nonce = nonce;
        self
    }

    /// Sets the clock that `created` is read from.
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Builds the signer.
    ///
    /// # Errors
    ///
    /// Returns [`WebBotAuthError::InvalidConfig`] when the `Signature-Agent` value is not an
    /// http(s) origin, the label or member key is not a structured-field key, or the components
    /// do not include `@authority` or `@target-uri`.
    pub fn build(self) -> Result<HttpSignatureSigner, WebBotAuthError> {
        let invalid = |reason: String| WebBotAuthError::InvalidConfig { reason };
        let url = reqwest::Url::parse(&self.signature_agent)
            .map_err(|e| invalid(format!("Signature-Agent `{}`: {e}", self.signature_agent)))?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid(format!(
                "Signature-Agent `{}` must be an origin such as https://bot.example",
                self.signature_agent
            )));
        }
        let agent_member = self.agent_member.unwrap_or_else(|| self.label.clone());
        for key in [&self.label, &agent_member] {
            let mut probe = Dictionary::new();
            probe.insert(key.clone(), ListEntry::Item(Item::new(BareItem::Integer(1))));
            probe.serialize().map_err(|_| {
                invalid(format!("`{key}` is not a structured-field key (lowercase, digits, _-.*)"))
            })?;
        }
        if !self.components.iter().any(|c| {
            (c.name() == "@authority" || c.name() == "@target-uri") && c.params().is_empty()
        }) {
            return Err(invalid("the components must include @authority or @target-uri".into()));
        }
        let own_member = ComponentId::signature_agent(agent_member.clone());
        if self.components.iter().any(|c| c.same_as(&own_member)) {
            return Err(invalid(format!(
                "the Signature-Agent member `{agent_member}` is covered automatically"
            )));
        }
        Ok(HttpSignatureSigner {
            key: self.key,
            signature_agent: self.signature_agent,
            label: self.label,
            agent_member,
            components: self.components,
            lifetime: self.lifetime,
            nonce: self.nonce,
            clock: self.clock,
        })
    }
}
