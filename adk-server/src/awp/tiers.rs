//! Trust tiers from verified credentials (spec 3.5 §7.3, A-I6; AWP 2.0 §6).
//!
//! [`EdgeTiers`] runs every configured [`CredentialVerifier`] on a request and assigns the
//! highest tier its verified credentials earn:
//!
//! | Credential | Verifier | Requester id | Tier |
//! |---|---|---|---|
//! | Web Bot Auth signature | [`WebBotAuthTrustAssigner`] | `wba:{directory origin}#{thumbprint}` | `known`; `verified` when the directory is on the owner's verified list |
//! | OAuth client-credentials token | [`OAuthClientCredentials`] | `oauth:{issuer}#{client id}` | `known`, or the tier its scopes grant |
//! | Client certificate from the listener | [`MtlsCredentials`] | `mtls:{base64url SHA-256 of the DER}` | `known`; `verified` for an issuer on the verified list |
//! | Owner-issued API key | [`ApiKeyCredentials`] | `key:{key id}` | `known` |
//! | Any of the above on an owner allowlist | [`TrustRules`] | — | the rule's tier, such as `partner` |
//!
//! Claims — `User-Agent`, `X-AWP-Channel`, any self-declared header — never raise a tier; they
//! become a `claimed_agent` label for analytics. A presented credential that fails a check is
//! refused with `unauthorized` (AWP 2.0 §6.4); one that cannot be checked, such as a signature
//! whose key directory is unreachable, earns `public`. Paired devices — `internal` in AWP 1.x —
//! are never recognised at the public edge.
//!
//! Limits key on the verified requester id, else a digest of the presented credential, else
//! the peer address ([`EdgeCaller::limit_key`]).

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Arc;

use adk_auth::sso::TokenValidator;
use adk_auth::web_bot_auth::{
    HttpSignatureVerifier, RequestHead, SIGNATURE_AGENT, SIGNATURE_INPUT,
};
use adk_awp::v2::tier::{TierAssigner, TierAssignment, claimed_agent};
use async_trait::async_trait;
use awp_types::{IdentityMethod, RequesterId, TrustRule, TrustRules, TrustTier};
use axum::extract::ConnectInfo;
use axum::http::request::Parts;
use axum::http::{HeaderMap, header};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest as _, Sha256};

/// The caller of one edge request, as [`EdgeTiers`] assessed it (spec 3.5 §7.3).
///
/// The edge stores it in the request's extensions, so every binding sees one assessment.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EdgeCaller {
    /// The caller's tier.
    pub tier: TrustTier,
    /// The verified requester id, from `known` up.
    pub requester: Option<RequesterId>,
    /// What limits key on: `id:{requester}`, `cred:{digest}` or `ip:{peer address}`.
    pub limit_key: String,
    /// The rate class of the owner's rule that matched, if any.
    pub rate_class: Option<String>,
    /// What the request claims to be, for analytics only.
    pub claimed_agent: Option<String>,
}

impl EdgeCaller {
    /// A `public` caller limited by `limit_key`.
    pub fn public(limit_key: impl Into<String>) -> Self {
        Self {
            tier: TrustTier::Public,
            requester: None,
            limit_key: limit_key.into(),
            rate_class: None,
            claimed_agent: None,
        }
    }

    /// The tier assignment the `/awp/v2/` routes read.
    pub fn assignment(&self) -> TierAssignment {
        match &self.requester {
            Some(requester) if self.tier > TrustTier::Public => {
                TierAssignment::verified(self.tier, requester.clone())
            }
            _ => TierAssignment::public(),
        }
    }
}

/// A presented credential that failed a check (AWP 2.0 §6.4): the request is refused with
/// `unauthorized`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{method:?} credential failed verification: {reason}")]
pub struct EdgeRefusal {
    /// The identity method whose credential failed.
    pub method: IdentityMethod,
    /// Why, for logs; never sent to the requester.
    pub reason: String,
}

/// A credential a verifier proved (spec 3.5 §7.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedCredential {
    /// The requester id.
    pub requester: RequesterId,
    /// Who vouches for it — a key directory origin, an OAuth issuer, a certificate issuer —
    /// matched against the owner's verified list.
    pub issuer: String,
    /// A tier the credential itself grants, such as an OAuth scope mapped to `partner`.
    pub granted: Option<TrustTier>,
}

/// What a [`CredentialVerifier`] found on a request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CredentialCheck {
    /// The request presents no credential of this kind.
    Absent,
    /// The credential verified.
    Verified(VerifiedCredential),
    /// The credential could not be checked, for example because its key directory is
    /// unreachable; the request stays `public` and limits key on `digest`.
    Unverifiable {
        /// A digest of the presented credential.
        digest: String,
        /// Why, for logs.
        reason: String,
    },
    /// The credential failed a check; the request is refused.
    Failed {
        /// Why, for logs.
        reason: String,
    },
}

/// Verifies one kind of credential (spec 3.5 §7.3).
///
/// # Example
///
/// ```rust
/// use adk_server::awp::tiers::{CredentialCheck, CredentialVerifier, VerifiedCredential};
/// use awp_types::{IdentityMethod, RequesterId};
///
/// /// Trusts a header set by a gateway that already authenticated the caller.
/// struct GatewayHeader;
///
/// #[async_trait::async_trait]
/// impl CredentialVerifier for GatewayHeader {
///     fn method(&self) -> IdentityMethod {
///         IdentityMethod::ApiKey
///     }
///
///     async fn verify(&self, head: &axum::http::request::Parts) -> CredentialCheck {
///         match head.headers.get("x-gateway-client").and_then(|value| value.to_str().ok()) {
///             None => CredentialCheck::Absent,
///             Some(id) => CredentialCheck::Verified(VerifiedCredential {
///                 requester: RequesterId::api_key(id),
///                 issuer: "gateway".into(),
///                 granted: None,
///             }),
///         }
///     }
/// }
/// ```
#[async_trait]
pub trait CredentialVerifier: Send + Sync {
    /// The identity method this verifier accepts, published in the trust rules.
    fn method(&self) -> IdentityMethod;

    /// Checks the request's credential of this kind.
    async fn verify(&self, head: &Parts) -> CredentialCheck;
}

fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0x1f]);
    }
    hex::encode(&hasher.finalize()[..16])
}

fn header_str(headers: &HeaderMap, name: impl axum::http::header::AsHeaderName) -> &str {
    headers.get(name).and_then(|value| value.to_str().ok()).unwrap_or("")
}

// --- Web Bot Auth --------------------------------------------------------------------------

/// Web Bot Auth at the edge: `known` for a verified signature, `verified` when its key
/// directory is on the owner's verified list, and an owner rule's tier, such as `partner`
/// (spec 3.5 §7.3; AWP 2.0 §6.3).
///
/// It is a [`CredentialVerifier`] inside [`EdgeTiers`] and, alone, a [`TierAssigner`]. The
/// [`HttpSignatureVerifier`] requires the draft's covered components, `created` and `expires`
/// within its skew (60 s by default), and caches directories for their HTTP cache lifetime.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_auth::web_bot_auth::{HttpSignatureVerifier, InMemoryKeyDirectorySource};
/// use adk_server::awp::tiers::WebBotAuthTrustAssigner;
///
/// let verifier = HttpSignatureVerifier::builder(Arc::new(InMemoryKeyDirectorySource::new())).build();
/// let assigner = WebBotAuthTrustAssigner::new(verifier).verified_directory("https://bots.example.com");
/// # let _ = assigner;
/// ```
pub struct WebBotAuthTrustAssigner {
    verifier: HttpSignatureVerifier,
    verified: BTreeSet<String>,
    rules: TrustRules,
    default_scheme: String,
}

impl std::fmt::Debug for WebBotAuthTrustAssigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebBotAuthTrustAssigner")
            .field("verified", &self.verified)
            .finish_non_exhaustive()
    }
}

impl WebBotAuthTrustAssigner {
    /// An assigner over `verifier`, with no verified directories and no rules.
    pub fn new(verifier: HttpSignatureVerifier) -> Self {
        Self {
            verifier,
            verified: BTreeSet::new(),
            rules: TrustRules::default(),
            default_scheme: "https".to_owned(),
        }
    }

    /// Adds a key directory origin to the owner's verified list.
    #[must_use]
    pub fn verified_directory(mut self, origin: impl Into<String>) -> Self {
        self.verified.insert(origin.into());
        self
    }

    /// Sets the owner's rules, used when the assigner runs alone.
    #[must_use]
    pub fn rules(mut self, rules: TrustRules) -> Self {
        self.rules = rules;
        self
    }

    /// The scheme of an origin-form request URI, `https` by default; set `http` behind a
    /// plain-text listener in tests.
    #[must_use]
    pub fn default_scheme(mut self, scheme: impl Into<String>) -> Self {
        self.default_scheme = scheme.into();
        self
    }
}

#[async_trait]
impl CredentialVerifier for WebBotAuthTrustAssigner {
    fn method(&self) -> IdentityMethod {
        IdentityMethod::WebBotAuth
    }

    async fn verify(&self, head: &Parts) -> CredentialCheck {
        let request = RequestHead::new(&head.method, &head.uri, &head.headers)
            .with_default_scheme(&self.default_scheme);
        match self.verifier.verify(request).await {
            Ok(None) => CredentialCheck::Absent,
            Ok(Some(requester)) => {
                let tier = self.verified.contains(&requester.directory_origin);
                CredentialCheck::Verified(VerifiedCredential {
                    requester: RequesterId::new(requester.id),
                    issuer: requester.directory_origin,
                    granted: tier.then_some(TrustTier::Verified),
                })
            }
            Err(error) if error.is_unverified() => CredentialCheck::Unverifiable {
                digest: digest(&[
                    "wba",
                    header_str(&head.headers, SIGNATURE_AGENT),
                    header_str(&head.headers, SIGNATURE_INPUT),
                ]),
                reason: error.to_string(),
            },
            Err(error) => CredentialCheck::Failed { reason: error.to_string() },
        }
    }
}

#[async_trait]
impl TierAssigner for WebBotAuthTrustAssigner {
    async fn assign(&self, head: &Parts) -> TierAssignment {
        match CredentialVerifier::verify(self, head).await {
            CredentialCheck::Verified(credential) => {
                let tier = credential
                    .granted
                    .unwrap_or(TrustTier::Known)
                    .max(self.rules.tier_for(&credential.requester));
                TierAssignment::verified(tier, credential.requester)
            }
            _ => TierAssignment::public(),
        }
    }
}

// --- OAuth client credentials --------------------------------------------------------------

/// OAuth 2.0 client-credentials tokens through `adk-auth`'s validators: `known`, or the tier
/// the token's scopes grant (spec 3.5 §7.3; AWP 2.0 §6.2).
///
/// A bearer token from an issuer with no validator, or one that fails validation, is refused.
/// The client id is the token's `client_id` claim, else `azp`, else `sub`.
///
/// # Example
///
/// ```rust,ignore
/// use adk_auth::sso::JwtValidator;
/// use adk_server::awp::tiers::OAuthClientCredentials;
/// use awp_types::TrustTier;
///
/// let validator = JwtValidator::builder()
///     .issuer("https://auth.partner.example")
///     .jwks_uri("https://auth.partner.example/.well-known/jwks.json")
///     .build()?;
/// let oauth = OAuthClientCredentials::new()
///     .validator(std::sync::Arc::new(validator))
///     .scope_tier("awp:partner", TrustTier::Partner);
/// ```
#[derive(Default)]
pub struct OAuthClientCredentials {
    validators: BTreeMap<String, Arc<dyn TokenValidator>>,
    scope_tiers: BTreeMap<String, TrustTier>,
}

impl std::fmt::Debug for OAuthClientCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthClientCredentials")
            .field("issuers", &self.validators.keys().collect::<Vec<_>>())
            .field("scope_tiers", &self.scope_tiers)
            .finish()
    }
}

impl OAuthClientCredentials {
    /// Accepts no issuer until [`validator`](Self::validator) adds one.
    pub fn new() -> Self {
        Self::default()
    }

    /// Accepts tokens `validator` validates, for its issuer.
    #[must_use]
    pub fn validator(mut self, validator: Arc<dyn TokenValidator>) -> Self {
        self.validators.insert(validator.issuer().to_owned(), validator);
        self
    }

    /// Grants `tier` to tokens carrying `scope`.
    #[must_use]
    pub fn scope_tier(mut self, scope: impl Into<String>, tier: TrustTier) -> Self {
        self.scope_tiers.insert(scope.into(), tier);
        self
    }

    /// The unverified `iss` of a JWT, to choose its validator.
    fn issuer_of(token: &str) -> Option<String> {
        let payload = token.split('.').nth(1)?;
        let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
        let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        claims.get("iss")?.as_str().map(str::to_owned)
    }
}

#[async_trait]
impl CredentialVerifier for OAuthClientCredentials {
    fn method(&self) -> IdentityMethod {
        IdentityMethod::Oauth2ClientCredentials
    }

    async fn verify(&self, head: &Parts) -> CredentialCheck {
        let Some(token) = header_str(&head.headers, header::AUTHORIZATION)
            .strip_prefix("Bearer ")
            .map(str::trim)
            .filter(|token| !token.is_empty())
        else {
            return CredentialCheck::Absent;
        };
        let Some(validator) = Self::issuer_of(token).and_then(|iss| self.validators.get(&iss))
        else {
            return CredentialCheck::Failed {
                reason: "the bearer token's issuer is not one the site trusts".to_owned(),
            };
        };
        match validator.validate(token).await {
            Ok(claims) => {
                let client = claims
                    .get_custom::<String>("client_id")
                    .or_else(|| claims.get_custom::<String>("azp"))
                    .unwrap_or_else(|| claims.sub.clone());
                let granted = claims
                    .scopes()
                    .iter()
                    .filter_map(|scope| self.scope_tiers.get(scope).copied())
                    .max();
                CredentialCheck::Verified(VerifiedCredential {
                    requester: RequesterId::oauth_client(&claims.iss, &client),
                    issuer: claims.iss,
                    granted,
                })
            }
            Err(error) => CredentialCheck::Failed { reason: error.to_string() },
        }
    }
}

// --- mTLS ----------------------------------------------------------------------------------

/// The client certificate a TLS listener verified, inserted into the request's extensions
/// by the listener (spec 3.5 §7.3; AWP 2.0 §6.2).
///
/// The edge reads it; it never parses or verifies certificates itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientCertificate {
    /// The certificate, DER-encoded.
    pub der: Vec<u8>,
    /// The issuer's distinguished name, matched against the owner's verified list.
    pub issuer: Option<String>,
}

/// Mutual TLS where the listener provides a client certificate: `known`, `verified` for an
/// issuer on the owner's verified list (spec 3.5 §7.3).
///
/// # Example
///
/// ```rust
/// use adk_server::awp::tiers::MtlsCredentials;
///
/// let mtls = MtlsCredentials::new().trusted_issuer("CN=Partner CA");
/// # let _ = mtls;
/// ```
#[derive(Debug, Clone, Default)]
pub struct MtlsCredentials {
    trusted: BTreeSet<String>,
}

impl MtlsCredentials {
    /// Accepts every certificate the listener verified.
    pub fn new() -> Self {
        Self::default()
    }

    /// Accepts only certificates from these issuers; others are refused.
    #[must_use]
    pub fn trusted_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.trusted.insert(issuer.into());
        self
    }
}

#[async_trait]
impl CredentialVerifier for MtlsCredentials {
    fn method(&self) -> IdentityMethod {
        IdentityMethod::Mtls
    }

    async fn verify(&self, head: &Parts) -> CredentialCheck {
        let Some(certificate) = head.extensions.get::<ClientCertificate>() else {
            return CredentialCheck::Absent;
        };
        let issuer = certificate.issuer.clone().unwrap_or_default();
        if !self.trusted.is_empty() && !self.trusted.contains(&issuer) {
            return CredentialCheck::Failed {
                reason: "the client certificate's issuer is not trusted".to_owned(),
            };
        }
        let thumbprint = URL_SAFE_NO_PAD.encode(Sha256::digest(&certificate.der));
        CredentialCheck::Verified(VerifiedCredential {
            requester: RequesterId::mtls(&thumbprint),
            issuer,
            granted: None,
        })
    }
}

// --- API keys ------------------------------------------------------------------------------

/// The authorization scheme of an owner-issued API key: `Authorization: ApiKey {id}:{secret}`.
pub const API_KEY_SCHEME: &str = "ApiKey";

/// API keys the owner issued: `known` (spec 3.5 §7.3). Only SHA-256 digests of the secrets
/// are held.
///
/// # Example
///
/// ```rust
/// use adk_server::awp::tiers::ApiKeyCredentials;
///
/// let keys = ApiKeyCredentials::new().key_secret("buyer-7", "s3cr3t-value");
/// # let _ = keys;
/// ```
#[derive(Debug, Clone, Default)]
pub struct ApiKeyCredentials {
    digests: BTreeMap<String, [u8; 32]>,
}

impl ApiKeyCredentials {
    /// Accepts no key until one is added.
    pub fn new() -> Self {
        Self::default()
    }

    /// Accepts key `id` whose secret has SHA-256 `digest`.
    #[must_use]
    pub fn key(mut self, id: impl Into<String>, digest: [u8; 32]) -> Self {
        self.digests.insert(id.into(), digest);
        self
    }

    /// Accepts key `id` with `secret`, keeping only its digest.
    #[must_use]
    pub fn key_secret(self, id: impl Into<String>, secret: &str) -> Self {
        self.key(id, Sha256::digest(secret.as_bytes()).into())
    }
}

#[async_trait]
impl CredentialVerifier for ApiKeyCredentials {
    fn method(&self) -> IdentityMethod {
        IdentityMethod::ApiKey
    }

    async fn verify(&self, head: &Parts) -> CredentialCheck {
        let value = header_str(&head.headers, header::AUTHORIZATION);
        let Some(key) = value
            .strip_prefix(API_KEY_SCHEME)
            .and_then(|rest| rest.strip_prefix(' '))
            .map(str::trim)
        else {
            return CredentialCheck::Absent;
        };
        let Some((id, secret)) = key.split_once(':') else {
            return CredentialCheck::Failed { reason: "an API key is `{id}:{secret}`".to_owned() };
        };
        let presented: [u8; 32] = Sha256::digest(secret.as_bytes()).into();
        let matches = self.digests.get(id).is_some_and(|expected| {
            expected.iter().zip(presented.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
        });
        if matches {
            CredentialCheck::Verified(VerifiedCredential {
                requester: RequesterId::api_key(id),
                issuer: "owner".to_owned(),
                granted: None,
            })
        } else {
            CredentialCheck::Failed { reason: format!("API key `{id}` is not valid") }
        }
    }
}

// --- The edge's assigner -------------------------------------------------------------------

/// Assigns each request's tier from its verified credentials and the owner's rules (spec 3.5
/// §7.3, A-I6; AWP 2.0 §6.5).
///
/// The tier of a verified requester is the highest of `known`, what its credential grants,
/// `verified` when its issuer is on the owner's verified list, and the highest owner rule its
/// id matches. Published rules appear at `GET /awp/v2/trust`; private rules, such as a partner
/// allowlist, apply without being published.
///
/// # Example
///
/// ```rust
/// use adk_server::awp::tiers::{ApiKeyCredentials, EdgeTiers};
/// use awp_types::{TrustRule, TrustTier};
///
/// let tiers = EdgeTiers::builder()
///     .verifier(ApiKeyCredentials::new().key_secret("partner-1", "secret"))
///     .private_rule(TrustRule {
///         subject: "key:partner-1".into(),
///         tier: TrustTier::Partner,
///         rate_class: None,
///         note: None,
///     })
///     .build();
/// assert!(tiers.published_rules().rules.is_empty());
/// ```
pub struct EdgeTiers {
    verifiers: Vec<Arc<dyn CredentialVerifier>>,
    rules: TrustRules,
    published: TrustRules,
    verified_issuers: BTreeSet<String>,
}

impl std::fmt::Debug for EdgeTiers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EdgeTiers")
            .field("accepts", &self.published.accepts)
            .field("rules", &self.rules.rules.len())
            .finish_non_exhaustive()
    }
}

/// Builder for [`EdgeTiers`].
#[derive(Default)]
pub struct EdgeTiersBuilder {
    verifiers: Vec<Arc<dyn CredentialVerifier>>,
    public_rules: Vec<TrustRule>,
    private_rules: Vec<TrustRule>,
    rate_classes: BTreeMap<String, awp_types::RateClass>,
    verified_issuers: BTreeSet<String>,
}

impl EdgeTiersBuilder {
    /// Adds a credential verifier.
    #[must_use]
    pub fn verifier(mut self, verifier: impl CredentialVerifier + 'static) -> Self {
        self.verifiers.push(Arc::new(verifier));
        self
    }

    /// Adds a shared credential verifier.
    #[must_use]
    pub fn shared_verifier(mut self, verifier: Arc<dyn CredentialVerifier>) -> Self {
        self.verifiers.push(verifier);
        self
    }

    /// Adds a published owner rule.
    #[must_use]
    pub fn rule(mut self, rule: TrustRule) -> Self {
        self.public_rules.push(rule);
        self
    }

    /// Adds an owner rule that applies without being published, such as a partner allowlist.
    #[must_use]
    pub fn private_rule(mut self, rule: TrustRule) -> Self {
        self.private_rules.push(rule);
        self
    }

    /// Defines a rate class rules and capabilities name.
    #[must_use]
    pub fn rate_class(mut self, name: impl Into<String>, class: awp_types::RateClass) -> Self {
        self.rate_classes.insert(name.into(), class);
        self
    }

    /// Adds an issuer — key directory origin, OAuth issuer, certificate issuer — to the owner's
    /// verified list.
    #[must_use]
    pub fn verified_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.verified_issuers.insert(issuer.into());
        self
    }

    /// Builds the assigner.
    pub fn build(self) -> EdgeTiers {
        let mut accepts: Vec<IdentityMethod> =
            self.verifiers.iter().map(|verifier| verifier.method()).collect();
        accepts.sort();
        accepts.dedup();
        let published = TrustRules {
            accepts: accepts.clone(),
            rules: self.public_rules.clone(),
            rate_classes: self.rate_classes.clone(),
        };
        let mut rules = published.clone();
        rules.rules.extend(self.private_rules);
        EdgeTiers {
            verifiers: self.verifiers,
            rules,
            published,
            verified_issuers: self.verified_issuers,
        }
    }
}

impl EdgeTiers {
    /// Starts a builder with no verifiers: every caller is `public` until one is added.
    pub fn builder() -> EdgeTiersBuilder {
        EdgeTiersBuilder::default()
    }

    /// The rules served at `GET /awp/v2/trust`.
    pub fn published_rules(&self) -> &TrustRules {
        &self.published
    }

    /// The rules applied, published and private.
    pub fn rules(&self) -> &TrustRules {
        &self.rules
    }

    /// The rate class of the highest rule `requester` matches.
    fn rate_class_for(&self, requester: &RequesterId) -> Option<String> {
        self.rules
            .rules
            .iter()
            .filter(|rule| rule.matches(requester))
            .max_by_key(|rule| rule.tier)
            .and_then(|rule| rule.rate_class.clone())
    }

    /// Assesses a request: its tier, requester and limit key, or the refusal of a credential
    /// that failed a check (AWP 2.0 §6.4).
    ///
    /// # Errors
    ///
    /// [`EdgeRefusal`] when a presented credential fails verification.
    pub async fn assess(&self, head: &Parts) -> Result<EdgeCaller, EdgeRefusal> {
        let mut best: Option<(TrustTier, RequesterId)> = None;
        let mut unverifiable: Option<String> = None;
        for verifier in &self.verifiers {
            match verifier.verify(head).await {
                CredentialCheck::Absent => {}
                CredentialCheck::Failed { reason } => {
                    tracing::info!(
                        awp.identity = ?verifier.method(),
                        reason = %reason,
                        "awp.request.refused: a credential failed verification"
                    );
                    return Err(EdgeRefusal { method: verifier.method(), reason });
                }
                CredentialCheck::Unverifiable { digest, reason } => {
                    tracing::info!(
                        awp.identity = ?verifier.method(),
                        reason = %reason,
                        "awp request credential could not be verified; serving as public"
                    );
                    unverifiable.get_or_insert(digest);
                }
                CredentialCheck::Verified(credential) => {
                    let mut tier = credential.granted.unwrap_or(TrustTier::Known);
                    if self.verified_issuers.contains(&credential.issuer) {
                        tier = tier.max(TrustTier::Verified);
                    }
                    tier = tier.max(self.rules.tier_for(&credential.requester));
                    if best.as_ref().is_none_or(|(current, _)| tier > *current) {
                        best = Some((tier, credential.requester));
                    }
                }
            }
        }
        let claimed = claimed_agent(&head.headers);
        Ok(match best {
            Some((tier, requester)) => {
                tracing::debug!(awp.requester = %requester.as_str(), awp.tier = %tier, "awp.request.verified");
                EdgeCaller {
                    tier,
                    rate_class: self.rate_class_for(&requester),
                    limit_key: format!("id:{}", requester.as_str()),
                    requester: Some(requester),
                    claimed_agent: claimed,
                }
            }
            None => {
                let limit_key = match unverifiable {
                    Some(digest) => format!("cred:{digest}"),
                    None => head.extensions.get::<ConnectInfo<SocketAddr>>().map_or_else(
                        || "ip:unknown".to_owned(),
                        |info| format!("ip:{}", info.0.ip()),
                    ),
                };
                EdgeCaller { claimed_agent: claimed, ..EdgeCaller::public(limit_key) }
            }
        })
    }
}

#[async_trait]
impl TierAssigner for EdgeTiers {
    /// Reads the edge's assessment from the request's extensions, else assesses the request;
    /// a refused credential is `public` here, as the edge refuses the request before any route.
    async fn assign(&self, head: &Parts) -> TierAssignment {
        if let Some(caller) = head.extensions.get::<EdgeCaller>() {
            return caller.assignment();
        }
        match self.assess(head).await {
            Ok(caller) => caller.assignment(),
            Err(_) => TierAssignment::public(),
        }
    }
}
