//! Verifies Web Bot Auth request signatures. Spec 3.5 §7.3, A-I6.

use std::sync::Arc;
use std::time::Duration;

use adk_core::{Clock, SystemClock};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::directory::{DirectoryCache, DirectoryCachePolicy, KeyDirectorySource};
use super::keys::{RFC9421_TEST_KEY_X, verify_ed25519};
use super::message::{ComponentId, RequestHead, SignatureInput};
use super::structured::{BareItem, Dictionary, Item, ListEntry};
use super::{
    SIGNATURE, SIGNATURE_AGENT, SIGNATURE_INPUT, WEB_BOT_AUTH_TAG, WebBotAuthError, combined_field,
};

/// At most this many Web Bot Auth signatures are checked per request.
const MAX_SIGNATURES: usize = 8;

/// A requester whose Web Bot Auth signature verified against the key directory its
/// `Signature-Agent` names. Spec 3.5 §7.3.
///
/// `id` is `wba:{directory origin}#{key thumbprint}`: the directory origin is what the
/// requester's identity rests on, and the thumbprint is the key that signed. The trust tier it
/// earns is the edge's decision, not this type's.
///
/// # Example
///
/// ```rust
/// use adk_auth::web_bot_auth::VerifiedRequester;
/// use chrono::{TimeZone, Utc};
///
/// let requester = VerifiedRequester::new(
///     "https://bot.example",
///     "poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U",
///     Utc.timestamp_opt(1_735_689_600, 0).unwrap(),
///     Utc.timestamp_opt(1_735_689_900, 0).unwrap(),
/// );
/// assert_eq!(requester.id, "wba:https://bot.example#poqkLGiymh_W0uP6PZFw-dvez3QJT5SolqXBCW38r0U");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedRequester {
    /// `wba:{directory origin}#{key thumbprint}`.
    pub id: String,
    /// The ASCII-serialised origin of the key directory.
    pub directory_origin: String,
    /// The JWK thumbprint of the key that signed (RFC 7638, RFC 8037).
    pub key_thumbprint: String,
    /// The signature's `created` time.
    pub created: DateTime<Utc>,
    /// The signature's `expires` time.
    pub expires: DateTime<Utc>,
}

impl VerifiedRequester {
    /// Creates a requester, deriving `id` from the origin and thumbprint.
    pub fn new(
        directory_origin: impl Into<String>,
        key_thumbprint: impl Into<String>,
        created: DateTime<Utc>,
        expires: DateTime<Utc>,
    ) -> Self {
        let directory_origin = directory_origin.into();
        let key_thumbprint = key_thumbprint.into();
        Self {
            id: format!("wba:{directory_origin}#{key_thumbprint}"),
            directory_origin,
            key_thumbprint,
            created,
            expires,
        }
    }
}

/// Verifies Web Bot Auth signatures ([RFC 9421] with draft-ietf-webbotauth-httpsig-protocol-00)
/// on incoming requests. Spec 3.5 §7.3.
///
/// For each signature tagged `web-bot-auth`, the verifier requires `created`, `expires`,
/// `keyid` and, when present, `alg="ed25519"`; requires `@authority` or `@target-uri` and the
/// signature's `Signature-Agent` member to be covered (draft §5.2, §5.2.1), plus any components
/// added with [`require_component`](HttpSignatureVerifierBuilder::require_component); accepts
/// `created` and `expires` within the clock skew (60 s by default); resolves the member's origin to
/// its key directory through the cache; selects the key whose thumbprint is `keyid`, keyed on
/// the (origin, key) pair (draft §5.4); and verifies the Ed25519 signature over the
/// recomputed signature base.
///
/// A request without a Web Bot Auth signature yields `Ok(None)`: no identity, whatever its
/// `User-Agent` claims (draft §6.11). When a request carries several, the first in
/// `Signature-Input` order that verifies is returned.
///
/// [RFC 9421]: https://www.rfc-editor.org/rfc/rfc9421
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_auth::web_bot_auth::{
///     Ed25519SigningKey, HttpSignatureSigner, HttpSignatureVerifier, InMemoryKeyDirectorySource,
///     KeyDirectory,
/// };
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let key = Arc::new(Ed25519SigningKey::generate().unwrap());
/// let directories = Arc::new(InMemoryKeyDirectorySource::new());
/// directories.insert("https://bot.example", KeyDirectory::from_keys([key.as_ref()]), None).await;
///
/// let signer = HttpSignatureSigner::builder(Arc::clone(&key), "https://bot.example").build().unwrap();
/// let mut request = http::Request::get("https://shop.example/catalog").body(()).unwrap();
/// signer.sign_request(&mut request).unwrap();
///
/// let verifier = HttpSignatureVerifier::builder(directories).build();
/// let requester = verifier.verify_request(&request).await.unwrap().unwrap();
/// assert_eq!(requester.id, format!("wba:https://bot.example#{}", key.thumbprint()));
///
/// let unsigned = http::Request::get("https://shop.example/").header("user-agent", "GPTBot").body(()).unwrap();
/// assert!(verifier.verify_request(&unsigned).await.unwrap().is_none());
/// # }
/// ```
#[derive(Debug)]
pub struct HttpSignatureVerifier {
    cache: DirectoryCache,
    clock: Arc<dyn Clock>,
    max_skew: Duration,
    max_lifetime: Option<Duration>,
    required: Vec<ComponentId>,
    default_scheme: String,
    allow_test_keys: bool,
}

impl HttpSignatureVerifier {
    /// Starts a builder resolving directories through `source`.
    pub fn builder(source: Arc<dyn KeyDirectorySource>) -> HttpSignatureVerifierBuilder {
        HttpSignatureVerifierBuilder {
            source,
            clock: Arc::new(SystemClock),
            cache_policy: DirectoryCachePolicy::default(),
            max_skew: Duration::from_secs(60),
            max_lifetime: Some(Duration::from_secs(24 * 60 * 60)),
            required: Vec::new(),
            default_scheme: "https".to_string(),
            allow_test_keys: false,
        }
    }

    /// Verifies an [`http::Request`], reading the scheme of an origin-form URI from the
    /// builder's [`default_scheme`](HttpSignatureVerifierBuilder::default_scheme).
    ///
    /// # Errors
    ///
    /// As [`verify`](Self::verify).
    pub async fn verify_request<B>(
        &self,
        request: &http::Request<B>,
    ) -> Result<Option<VerifiedRequester>, WebBotAuthError> {
        self.verify(RequestHead::from_request(request).with_default_scheme(&self.default_scheme))
            .await
    }

    /// Verifies the Web Bot Auth signatures on a request.
    ///
    /// Returns `Ok(None)` when the request carries no signature tagged `web-bot-auth`.
    ///
    /// # Errors
    ///
    /// Returns the first signature's error when no signature verifies.
    /// [`WebBotAuthError::is_unverified`] tells a signature whose key material was unavailable
    /// from one that failed a check.
    pub async fn verify(
        &self,
        head: RequestHead<'_>,
    ) -> Result<Option<VerifiedRequester>, WebBotAuthError> {
        let headers = head.headers();
        let combined = |name: &str| combined_field(headers, name);
        let inputs = combined(SIGNATURE_INPUT)?;
        let signatures = combined(SIGNATURE)?;
        let (inputs, signatures) = match (inputs, signatures) {
            (None, None) => return Ok(None),
            (Some(inputs), Some(signatures)) => (inputs, signatures),
            (Some(_), None) | (None, Some(_)) => {
                return Err(WebBotAuthError::MalformedField {
                    field: SIGNATURE.to_string(),
                    reason: "Signature and Signature-Input must appear together".into(),
                });
            }
        };
        let inputs = SignatureInput::parse_field(&inputs)?;
        let signatures = Dictionary::parse_unique(&signatures).map_err(|e| {
            WebBotAuthError::MalformedField { field: SIGNATURE.to_string(), reason: e.to_string() }
        })?;
        let agent_field = combined(SIGNATURE_AGENT)?;
        let tagged: Vec<_> = inputs
            .iter()
            .filter(|(_, input)| input.tag() == Some(WEB_BOT_AUTH_TAG))
            .take(MAX_SIGNATURES)
            .collect();
        if tagged.is_empty() {
            return Ok(None);
        }
        let now = self.clock.now().timestamp();
        let mut first_error = None;
        for (label, input) in tagged {
            let signature = match signatures.get(label) {
                Some(ListEntry::Item(Item {
                    bare_item: BareItem::ByteSequence(bytes), ..
                })) => bytes.as_slice(),
                Some(_) | None => {
                    first_error.get_or_insert(WebBotAuthError::MalformedField {
                        field: SIGNATURE.to_string(),
                        reason: format!(
                            "no byte-sequence member `{label}` matches Signature-Input"
                        ),
                    });
                    continue;
                }
            };
            match self.verify_one(&head, label, input, signature, agent_field.as_deref(), now).await
            {
                Ok(requester) => {
                    tracing::debug!(requester.id = %requester.id, signature.label = %label, "web bot auth signature verified");
                    return Ok(Some(requester));
                }
                Err(error) => {
                    tracing::debug!(signature.label = %label, error = %error, "web bot auth signature rejected");
                    first_error.get_or_insert(error);
                }
            }
        }
        Err(first_error.unwrap_or_else(|| WebBotAuthError::MalformedField {
            field: SIGNATURE.to_string(),
            reason: "no signature could be checked".into(),
        }))
    }

    async fn verify_one(
        &self,
        head: &RequestHead<'_>,
        label: &str,
        input: &SignatureInput,
        signature: &[u8],
        agent_field: Option<&str>,
        now: i64,
    ) -> Result<VerifiedRequester, WebBotAuthError> {
        let invalid = |parameter: &str, reason: &str| WebBotAuthError::InvalidParameter {
            label: label.to_string(),
            parameter: parameter.to_string(),
            reason: reason.to_string(),
        };
        if let Some(alg) = input.params.get("alg")
            && alg.as_str() != Some("ed25519")
        {
            return Err(WebBotAuthError::UnsupportedAlgorithm {
                label: label.to_string(),
                alg: alg.serialize().unwrap_or_default(),
            });
        }
        let created =
            input.created().ok_or_else(|| invalid("created", "missing or not an integer"))?;
        let expires =
            input.expires().ok_or_else(|| invalid("expires", "missing or not an integer"))?;
        let keyid = input.keyid().ok_or_else(|| invalid("keyid", "missing or not a string"))?;
        if expires < created {
            return Err(invalid("expires", "precedes created"));
        }

        let covers_target = input.components.iter().any(|c| {
            (c.name() == "@authority" || c.name() == "@target-uri") && c.params().is_empty()
        });
        if !covers_target {
            return Err(WebBotAuthError::MissingComponent {
                label: label.to_string(),
                component: "@authority or @target-uri".into(),
            });
        }
        if let Some(missing) = self.required.iter().find(|c| !input.covers(c)) {
            return Err(WebBotAuthError::MissingComponent {
                label: label.to_string(),
                component: missing.serialize().unwrap_or_else(|_| missing.name().to_string()),
            });
        }
        let agent = agent_component(label, input)?;

        let skew = i64::try_from(self.max_skew.as_secs()).unwrap_or(i64::MAX);
        if created > now.saturating_add(skew) {
            return Err(WebBotAuthError::NotYetValid { label: label.to_string(), created, now });
        }
        if now > expires.saturating_add(skew) {
            return Err(WebBotAuthError::Expired { label: label.to_string(), expires, now });
        }
        if let Some(max) = self.max_lifetime
            && (expires - created).unsigned_abs() > max.as_secs()
        {
            return Err(WebBotAuthError::LifetimeTooLong {
                label: label.to_string(),
                lifetime_secs: expires - created,
                max_secs: max.as_secs(),
            });
        }
        if signature.len() != 64 {
            return Err(WebBotAuthError::BadSignature { label: label.to_string() });
        }

        let origin = agent_origin(label, agent, agent_field)?;
        // Building the base first rejects a request that cannot verify before any fetch.
        let base = head.signature_base(input)?;
        let key = self.cache.key(&origin, keyid).await?;
        if !self.allow_test_keys && key.x == RFC9421_TEST_KEY_X {
            return Err(WebBotAuthError::TestKey { keyid: keyid.to_string() });
        }
        if !key.is_current(now) {
            return Err(WebBotAuthError::KeyNotCurrent { origin, keyid: keyid.to_string() });
        }
        let public_key = key.public_key()?;
        if !verify_ed25519(&public_key, base.as_bytes(), signature) {
            return Err(WebBotAuthError::BadSignature { label: label.to_string() });
        }
        let timestamp = |value: i64, parameter: &str| {
            DateTime::from_timestamp(value, 0).ok_or_else(|| invalid(parameter, "out of range"))
        };
        Ok(VerifiedRequester::new(
            origin,
            keyid,
            timestamp(created, "created")?,
            timestamp(expires, "expires")?,
        ))
    }
}

/// The `Signature-Agent` component a signature covers: its only one, or the one keyed to its
/// label when it covers several (draft §5.2.1–§5.2.2).
fn agent_component<'a>(
    label: &str,
    input: &'a SignatureInput,
) -> Result<&'a ComponentId, WebBotAuthError> {
    let covered: Vec<_> = input.components.iter().filter(|c| c.name() == SIGNATURE_AGENT).collect();
    let chosen = match covered.as_slice() {
        [only] => Some(*only),
        _ => covered
            .iter()
            .copied()
            .find(|c| c.params().get("key").and_then(BareItem::as_str) == Some(label)),
    };
    chosen.ok_or_else(|| WebBotAuthError::MissingComponent {
        label: label.to_string(),
        component: format!("\"signature-agent\";key=\"{label}\""),
    })
}

/// The directory origin a covered `Signature-Agent` member names (draft §5.2.1, §5.5).
fn agent_origin(
    label: &str,
    component: &ComponentId,
    field: Option<&str>,
) -> Result<String, WebBotAuthError> {
    let unusable = |reason: String| WebBotAuthError::InvalidSignatureAgent {
        label: label.to_string(),
        reason,
    };
    let field = field.ok_or_else(|| unusable("the request has no Signature-Agent field".into()))?;
    let malformed = |e: super::structured::StructuredFieldError| WebBotAuthError::MalformedField {
        field: SIGNATURE_AGENT.to_string(),
        reason: e.to_string(),
    };
    let member = match component.params().get("key") {
        Some(BareItem::String(key)) => {
            match Dictionary::parse(field).map_err(malformed)?.get(key) {
                Some(ListEntry::Item(item)) => item.clone(),
                Some(ListEntry::InnerList(_)) => {
                    return Err(unusable(format!("member `{key}` is not a string")));
                }
                None => return Err(unusable(format!("the field has no member `{key}`"))),
            }
        }
        Some(_) => return Err(unusable("the `key` parameter is not a string".into())),
        // The draft's legacy form: the whole field is one String (§5.2.1, Appendix E.2.2).
        None => Item::parse(field).map_err(malformed)?,
    };
    let value =
        member.bare_item.as_str().ok_or_else(|| unusable("the member is not a string".into()))?;
    match member.params.get("type") {
        None => {}
        Some(kind) if kind.as_token() == Some("directory") => {}
        Some(kind) => {
            return Err(WebBotAuthError::UnsupportedDiscovery {
                label: label.to_string(),
                kind: kind.serialize().unwrap_or_default(),
            });
        }
    }
    let url = reqwest::Url::parse(value).map_err(|e| unusable(format!("`{value}`: {e}")))?;
    let is_origin = matches!(url.scheme(), "https" | "http")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    if !is_origin {
        return Err(unusable(format!("`{value}` is not an http(s) origin")));
    }
    Ok(url.origin().ascii_serialization())
}

/// Builds an [`HttpSignatureVerifier`].
pub struct HttpSignatureVerifierBuilder {
    source: Arc<dyn KeyDirectorySource>,
    clock: Arc<dyn Clock>,
    cache_policy: DirectoryCachePolicy,
    max_skew: Duration,
    max_lifetime: Option<Duration>,
    required: Vec<ComponentId>,
    default_scheme: String,
    allow_test_keys: bool,
}

impl std::fmt::Debug for HttpSignatureVerifierBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSignatureVerifierBuilder")
            .field("max_skew", &self.max_skew)
            .field("max_lifetime", &self.max_lifetime)
            .finish_non_exhaustive()
    }
}

impl HttpSignatureVerifierBuilder {
    /// Sets the clock that `created`, `expires` and cache lifetimes are checked against.
    pub fn clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Sets the allowed clock skew for `created` and `expires`. Default 60 s.
    pub fn max_skew(mut self, skew: Duration) -> Self {
        self.max_skew = skew;
        self
    }

    /// Sets the longest `expires - created` accepted, or `None` for no limit. Default 24 h,
    /// the longest expiry the draft recommends (§5.2).
    pub fn max_signature_lifetime(mut self, lifetime: Option<Duration>) -> Self {
        self.max_lifetime = lifetime;
        self
    }

    /// Requires signatures to also cover `component`, such as `@method` or `@path`.
    pub fn require_component(mut self, component: ComponentId) -> Self {
        self.required.push(component);
        self
    }

    /// Sets the directory cache policy.
    pub fn cache_policy(mut self, policy: DirectoryCachePolicy) -> Self {
        self.cache_policy = policy;
        self
    }

    /// Sets the scheme of requests whose URI carries none. Default `https`.
    pub fn default_scheme(mut self, scheme: impl Into<String>) -> Self {
        self.default_scheme = scheme.into();
        self
    }

    /// Accepts the RFC 9421 Appendix B.1.4 test key, which is rejected by default (draft §6.8).
    /// For test vectors only.
    pub fn allow_test_keys(mut self, allow: bool) -> Self {
        self.allow_test_keys = allow;
        self
    }

    /// Builds the verifier.
    pub fn build(self) -> HttpSignatureVerifier {
        HttpSignatureVerifier {
            cache: DirectoryCache::new(self.source, self.cache_policy, Arc::clone(&self.clock)),
            clock: self.clock,
            max_skew: self.max_skew,
            max_lifetime: self.max_lifetime,
            required: self.required,
            default_scheme: self.default_scheme,
            allow_test_keys: self.allow_test_keys,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_bot_auth::InMemoryKeyDirectorySource;

    #[test]
    fn builder_defaults_match_spec_3_5_section_7_3() {
        let verifier =
            HttpSignatureVerifier::builder(Arc::new(InMemoryKeyDirectorySource::new())).build();
        assert_eq!(verifier.max_skew, Duration::from_secs(60));
        assert_eq!(verifier.max_lifetime, Some(Duration::from_secs(86_400)));
        assert_eq!(verifier.default_scheme, "https");
        assert!(!verifier.allow_test_keys);
        assert!(verifier.required.is_empty());
    }
}
