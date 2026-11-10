//! An OpenID Connect issuer for agents: a discovery document, a JWK set, and short-lived
//! RS256 tokens whose subject is the agent's [`AgentId`].
//!
//! AWS IAM, Amazon Bedrock AgentCore Identity, Google Cloud and Microsoft Entra fetch the
//! discovery document and the keys from the issuer URL over HTTPS, so the two documents
//! must be published at [`AgentTokenIssuer::DISCOVERY_PATH`] and
//! [`AgentTokenIssuer::JWKS_PATH`] below it.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use adk_core::{
    AgentCredential, AgentId, AgentPrincipal, CredentialKind, CredentialSecret, Url, async_trait,
};
use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::rsa::{KeyPair, KeySize};
use aws_lc_rs::signature::{KeyPair as _, RSA_PKCS1_SHA256};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use zeroize::Zeroizing;

use super::FederationError;

/// The JWS algorithm of every agent token: RSASSA-PKCS1-v1_5 with SHA-256.
pub const TOKEN_ALGORITHM: &str = "RS256";

/// Seconds a token's `nbf` precedes its `iat`, for verifiers whose clocks run behind.
const NOT_BEFORE_LEEWAY: i64 = 5;

/// An RSA public key as the issuer's JWK set publishes it (RFC 7517, RFC 7518 §6.3.1).
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::{AgentTokenSigner, RsaTokenSigner};
///
/// let signer = RsaTokenSigner::generate("agents-2026-11")?;
/// let jwk = serde_json::to_value(signer.public_jwk())?;
/// assert_eq!(jwk["kty"], "RSA");
/// assert_eq!(jwk["use"], "sig");
/// assert_eq!(jwk["alg"], "RS256");
/// assert_eq!(jwk["kid"], "agents-2026-11");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RsaPublicJwk {
    /// Key type: `RSA`.
    pub kty: String,
    /// Intended use: `sig`.
    #[serde(rename = "use")]
    pub key_use: String,
    /// Algorithm: `RS256`.
    pub alg: String,
    /// Key id, repeated in the header of every token the key signs.
    pub kid: String,
    /// The modulus, base64url without padding.
    pub n: String,
    /// The public exponent, base64url without padding.
    pub e: String,
}

/// The issuer's JWK set, served at [`AgentTokenIssuer::JWKS_PATH`].
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::{AgentTokenIssuer, RsaTokenSigner};
///
/// let issuer = AgentTokenIssuer::new(
///     "https://agents.example.com",
///     std::sync::Arc::new(RsaTokenSigner::generate("agents-2026-11")?),
/// )?;
/// assert_eq!(issuer.jwks().keys[0].kid, "agents-2026-11");
/// # Ok::<(), adk_auth::federation::FederationError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JwkSet {
    /// The keys tokens are verified with.
    pub keys: Vec<RsaPublicJwk>,
}

/// The issuer's OpenID Connect discovery document, served at
/// [`AgentTokenIssuer::DISCOVERY_PATH`].
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::{AgentTokenIssuer, RsaTokenSigner};
///
/// let issuer = AgentTokenIssuer::new(
///     "https://agents.example.com/",
///     std::sync::Arc::new(RsaTokenSigner::generate("agents-2026-11")?),
/// )?;
/// let document = issuer.discovery_document();
/// assert_eq!(document.issuer, "https://agents.example.com");
/// assert_eq!(document.jwks_uri, "https://agents.example.com/jwks.json");
/// assert_eq!(document.id_token_signing_alg_values_supported, ["RS256"]);
/// # Ok::<(), adk_auth::federation::FederationError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryDocument {
    /// The issuer URL, equal to the `iss` claim of every token.
    pub issuer: String,
    /// Where the JWK set is served.
    pub jwks_uri: String,
    /// `["id_token"]`.
    pub response_types_supported: Vec<String>,
    /// `["public"]`.
    pub subject_types_supported: Vec<String>,
    /// `["RS256"]`.
    pub id_token_signing_alg_values_supported: Vec<String>,
    /// The claims a token carries.
    pub claims_supported: Vec<String>,
}

/// A key that signs agent tokens with RS256; the private key never leaves it.
///
/// Implement it over a key service or hardware token; [`RsaTokenSigner`] holds the key in
/// memory.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::{AgentTokenSigner, FederationError, RsaPublicJwk, RsaTokenSigner};
/// use adk_core::async_trait;
///
/// /// Signs through a key service; a local signer stands in for its client here.
/// #[derive(Debug)]
/// struct KeyServiceSigner {
///     remote: RsaTokenSigner,
/// }
///
/// #[async_trait]
/// impl AgentTokenSigner for KeyServiceSigner {
///     fn kid(&self) -> &str {
///         self.remote.kid()
///     }
///
///     fn public_jwk(&self) -> RsaPublicJwk {
///         self.remote.public_jwk()
///     }
///
///     async fn sign(&self, signing_input: &[u8]) -> Result<Vec<u8>, FederationError> {
///         self.remote.sign(signing_input).await
///     }
/// }
/// ```
#[async_trait]
pub trait AgentTokenSigner: fmt::Debug + Send + Sync {
    /// The key id, placed in the `kid` header of every token.
    fn kid(&self) -> &str;

    /// The public key, as the JWK set publishes it.
    fn public_jwk(&self) -> RsaPublicJwk;

    /// Signs `signing_input` with RSASSA-PKCS1-v1_5 and SHA-256 (RFC 7518 §3.3).
    ///
    /// # Errors
    ///
    /// Returns [`FederationError::Signing`] when the key cannot sign.
    async fn sign(&self, signing_input: &[u8]) -> Result<Vec<u8>, FederationError>;
}

/// An RS256 [`AgentTokenSigner`] holding an RSA private key in memory.
///
/// Signing uses `aws-lc-rs`. `Debug` shows the key id only.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::{AgentTokenSigner, RsaTokenSigner};
///
/// let signer = RsaTokenSigner::generate("agents-2026-11")?;
/// assert_eq!(signer.kid(), "agents-2026-11");
/// assert_eq!(format!("{signer:?}"), r#"RsaTokenSigner { kid: "agents-2026-11", .. }"#);
/// # Ok::<(), adk_auth::federation::FederationError>(())
/// ```
pub struct RsaTokenSigner {
    kid: String,
    key_pair: KeyPair,
    public_jwk: RsaPublicJwk,
}

impl fmt::Debug for RsaTokenSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RsaTokenSigner").field("kid", &self.kid).finish_non_exhaustive()
    }
}

impl RsaTokenSigner {
    /// Creates a signer for key `kid` from an unencrypted PKCS#8 private key in DER form.
    ///
    /// # Errors
    ///
    /// Returns [`FederationError::InvalidKey`] when `kid` is empty or the bytes are not an
    /// RSA PKCS#8 private key of 2048 to 8192 bits.
    pub fn from_pkcs8_der(kid: impl Into<String>, der: &[u8]) -> Result<Self, FederationError> {
        let kid = kid.into();
        if kid.is_empty() {
            return Err(FederationError::InvalidKey { reason: "the key id is empty".into() });
        }
        let key_pair = KeyPair::from_pkcs8(der)
            .map_err(|error| FederationError::InvalidKey { reason: error.to_string() })?;
        let public = key_pair.public_key();
        let public_jwk = RsaPublicJwk {
            kty: "RSA".into(),
            key_use: "sig".into(),
            alg: TOKEN_ALGORITHM.into(),
            kid: kid.clone(),
            n: URL_SAFE_NO_PAD.encode(public.modulus().big_endian_without_leading_zero()),
            e: URL_SAFE_NO_PAD.encode(public.exponent().big_endian_without_leading_zero()),
        };
        Ok(Self { kid, key_pair, public_jwk })
    }

    /// Creates a signer for key `kid` from an unencrypted PKCS#8 private key in PEM form
    /// (`-----BEGIN PRIVATE KEY-----`).
    ///
    /// # Errors
    ///
    /// Returns [`FederationError::InvalidKey`] when the PEM is a PKCS#1 or an encrypted key,
    /// is not valid base64, or holds a key [`from_pkcs8_der`](Self::from_pkcs8_der) rejects.
    pub fn from_pkcs8_pem(kid: impl Into<String>, pem: &str) -> Result<Self, FederationError> {
        const BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
        const END: &str = "-----END PRIVATE KEY-----";
        let invalid = |reason: &str| FederationError::InvalidKey { reason: reason.to_string() };
        let body = pem
            .find(BEGIN)
            .map(|start| &pem[start + BEGIN.len()..])
            .and_then(|rest| rest.find(END).map(|end| &rest[..end]))
            .ok_or_else(|| {
                if pem.contains("BEGIN RSA PRIVATE KEY") {
                    invalid("the PEM holds a PKCS#1 key, not PKCS#8")
                } else if pem.contains("BEGIN ENCRYPTED PRIVATE KEY") {
                    invalid("the PEM holds an encrypted key")
                } else {
                    invalid("the PEM has no `PRIVATE KEY` block")
                }
            })?;
        let encoded: Zeroizing<String> =
            Zeroizing::new(body.chars().filter(|c| !c.is_ascii_whitespace()).collect());
        let der = Zeroizing::new(
            STANDARD
                .decode(encoded.as_bytes())
                .map_err(|_| invalid("the PEM body is not valid base64"))?,
        );
        Self::from_pkcs8_der(kid, &der)
    }

    /// Creates a signer for key `kid` with a fresh 2048-bit key that lives as long as the
    /// process, for tests and for issuers that publish their JWK set at startup.
    ///
    /// # Errors
    ///
    /// Returns [`FederationError::InvalidKey`] when `kid` is empty or key generation fails.
    pub fn generate(kid: impl Into<String>) -> Result<Self, FederationError> {
        let invalid = |_| FederationError::InvalidKey { reason: "key generation failed".into() };
        let der =
            KeyPair::generate(KeySize::Rsa2048).map_err(invalid)?.as_der().map_err(invalid)?;
        Self::from_pkcs8_der(kid, der.as_ref())
    }
}

#[async_trait]
impl AgentTokenSigner for RsaTokenSigner {
    fn kid(&self) -> &str {
        &self.kid
    }

    fn public_jwk(&self) -> RsaPublicJwk {
        self.public_jwk.clone()
    }

    async fn sign(&self, signing_input: &[u8]) -> Result<Vec<u8>, FederationError> {
        let mut signature = vec![0; self.key_pair.public_modulus_len()];
        self.key_pair
            .sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), signing_input, &mut signature)
            .map_err(|error| FederationError::Signing { reason: error.to_string() })?;
        Ok(signature)
    }
}

/// A signed token identifying one agent to one audience.
///
/// `Debug` never prints the token.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::AgentToken;
/// use adk_core::{AgentCredential, AgentId, CredentialKind};
///
/// let token = AgentToken::new(
///     AgentId::new("did:web:shop.example.com:agents:cashier")?,
///     "eyJhbGciOi...",
///     "sts.amazonaws.com",
///     chrono::Utc::now() + chrono::Duration::minutes(10),
/// );
/// assert!(!format!("{token:?}").contains("eyJ"));
///
/// let credential = AgentCredential::from(token);
/// assert_eq!(credential.kind, CredentialKind::JwtSvid);
/// assert_eq!(credential.audience, ["sts.amazonaws.com"]);
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentToken {
    /// The agent the token identifies: its `sub` claim.
    pub subject: AgentId,
    /// The compact JWS.
    pub secret: CredentialSecret,
    /// The party the token is addressed to: its `aud` claim.
    pub audience: String,
    /// When the token stops being valid: its `exp` claim.
    pub expires_at: DateTime<Utc>,
}

impl AgentToken {
    /// A token for `subject`, addressed to `audience`, valid until `expires_at`.
    pub fn new(
        subject: AgentId,
        secret: impl Into<String>,
        audience: impl Into<String>,
        expires_at: DateTime<Utc>,
    ) -> Self {
        Self {
            subject,
            secret: CredentialSecret::new(secret),
            audience: audience.into(),
            expires_at,
        }
    }
}

impl From<AgentToken> for AgentCredential {
    fn from(token: AgentToken) -> Self {
        AgentCredential::new(token.subject, CredentialKind::JwtSvid, token.secret.expose())
            .expires_at(token.expires_at)
            .with_audience(token.audience)
    }
}

/// Yields a token that identifies an agent to one audience.
///
/// The cloud exchanges ask it for the token they present. [`AgentTokenIssuer`] implements
/// it by signing one; implement it over a token another system issues, such as a projected
/// service-account token or a SPIFFE JWT-SVID.
///
/// # Example
///
/// ```rust
/// use adk_auth::federation::{AgentToken, AgentTokenSource, FederationError};
/// use adk_core::{AgentPrincipal, async_trait};
///
/// /// Reads the token a platform mounts for the workload.
/// #[derive(Debug)]
/// struct MountedToken {
///     path: std::path::PathBuf,
/// }
///
/// #[async_trait]
/// impl AgentTokenSource for MountedToken {
///     async fn agent_token(
///         &self,
///         agent: &AgentPrincipal,
///         audience: &str,
///     ) -> Result<AgentToken, FederationError> {
///         let jwt = std::fs::read_to_string(&self.path).map_err(|error| {
///             FederationError::Signing { reason: format!("no mounted token: {error}") }
///         })?;
///         let expires_at = chrono::Utc::now() + chrono::Duration::minutes(5);
///         Ok(AgentToken::new(agent.id.clone(), jwt.trim(), audience, expires_at))
///     }
/// }
/// ```
#[async_trait]
pub trait AgentTokenSource: fmt::Debug + Send + Sync {
    /// A token whose subject is `agent` and whose audience is `audience`.
    ///
    /// # Errors
    ///
    /// Returns an error when no token can be produced; the exchange fails with it and
    /// sends nothing.
    async fn agent_token(
        &self,
        agent: &AgentPrincipal,
        audience: &str,
    ) -> Result<AgentToken, FederationError>;
}

/// Issues short-lived RS256 tokens for agents and describes itself as an OpenID Connect
/// issuer.
///
/// | Claim | Value |
/// |-------|-------|
/// | `iss` | The issuer URL, without a trailing slash |
/// | `sub` | The agent's [`AgentId`] |
/// | `aud` | The audience the caller names |
/// | `iat`, `nbf` | The time of issue, and five seconds before it |
/// | `exp` | `iat` plus the token lifetime: 10 minutes by default, 1 hour at most |
/// | `jti` | A random UUID |
///
/// Serve [`discovery_document`](Self::discovery_document) at
/// [`DISCOVERY_PATH`](Self::DISCOVERY_PATH) and [`jwks`](Self::jwks) at
/// [`JWKS_PATH`](Self::JWKS_PATH) below the issuer URL, over HTTPS and without
/// authentication: each cloud fetches both itself.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_auth::federation::{AgentTokenIssuer, RsaTokenSigner};
/// use adk_core::{AgentId, AgentPrincipal};
///
/// # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
/// let issuer = AgentTokenIssuer::new(
///     "https://agents.example.com",
///     Arc::new(RsaTokenSigner::generate("agents-2026-11")?),
/// )?;
/// let cashier = AgentPrincipal::new(AgentId::new("did:web:shop.example.com:agents:cashier")?);
///
/// let token = issuer.issue(&cashier, "sts.amazonaws.com").await?;
/// assert_eq!(token.subject, cashier.id);
/// assert_eq!(token.secret.expose().split('.').count(), 3);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// # }).unwrap();
/// ```
#[derive(Debug, Clone)]
pub struct AgentTokenIssuer {
    issuer: String,
    signer: Arc<dyn AgentTokenSigner>,
    token_ttl: Duration,
}

impl AgentTokenIssuer {
    /// The path of the discovery document below the issuer URL.
    pub const DISCOVERY_PATH: &'static str = "/.well-known/openid-configuration";

    /// The path of the JWK set below the issuer URL.
    pub const JWKS_PATH: &'static str = "/jwks.json";

    /// The lifetime of a token unless [`with_token_ttl`](Self::with_token_ttl) sets another.
    pub const DEFAULT_TOKEN_TTL: Duration = Duration::from_secs(600);

    /// The longest lifetime a token is issued with.
    pub const MAX_TOKEN_TTL: Duration = Duration::from_secs(3600);

    /// An issuer at `issuer_url` signing with `signer`. A trailing slash is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`FederationError::InvalidConfig`] when `issuer_url` is not an absolute
    /// `https` URL without query, fragment or user information. `http` is accepted for
    /// `localhost` and loopback addresses only.
    pub fn new(
        issuer_url: impl Into<String>,
        signer: Arc<dyn AgentTokenSigner>,
    ) -> Result<Self, FederationError> {
        let issuer = issuer_url.into().trim_end_matches('/').to_string();
        let invalid = |why: &str| FederationError::InvalidConfig {
            reason: format!(
                "issuer URL '{issuer}' {why}; use an https URL such as 'https://agents.example.com'"
            ),
        };
        let url =
            Url::parse(&issuer).map_err(|error| invalid(&format!("is not a URL ({error})")))?;
        let Some(host) = url.host_str() else {
            return Err(invalid("names no host"));
        };
        let loopback = host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        match url.scheme() {
            "https" => {}
            "http" if loopback => {}
            _ => return Err(invalid("must use https, which every cloud requires of an issuer")),
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err(invalid("must not carry a query or a fragment"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(invalid("must not carry user information"));
        }
        Ok(Self { issuer, signer, token_ttl: Self::DEFAULT_TOKEN_TTL })
    }

    /// An issuer at `issuer_url` signing with the PKCS#8 PEM private key `pem` under key id
    /// `kid`.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`RsaTokenSigner::from_pkcs8_pem`] and [`new`](Self::new).
    pub fn from_rsa_pem(
        issuer_url: impl Into<String>,
        kid: impl Into<String>,
        pem: &str,
    ) -> Result<Self, FederationError> {
        Self::new(issuer_url, Arc::new(RsaTokenSigner::from_pkcs8_pem(kid, pem)?))
    }

    /// An issuer at `issuer_url` signing with the PKCS#8 DER private key `der` under key id
    /// `kid`.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`RsaTokenSigner::from_pkcs8_der`] and [`new`](Self::new).
    pub fn from_rsa_der(
        issuer_url: impl Into<String>,
        kid: impl Into<String>,
        der: &[u8],
    ) -> Result<Self, FederationError> {
        Self::new(issuer_url, Arc::new(RsaTokenSigner::from_pkcs8_der(kid, der)?))
    }

    /// Issues tokens valid for `ttl`, held between one second and
    /// [`MAX_TOKEN_TTL`](Self::MAX_TOKEN_TTL).
    #[must_use]
    pub fn with_token_ttl(mut self, ttl: Duration) -> Self {
        self.token_ttl = ttl.clamp(Duration::from_secs(1), Self::MAX_TOKEN_TTL);
        self
    }

    /// The issuer URL: the `iss` claim of every token.
    pub fn issuer(&self) -> &str {
        &self.issuer
    }

    /// The lifetime of the tokens this issuer signs.
    pub fn token_ttl(&self) -> Duration {
        self.token_ttl
    }

    /// The OpenID Connect discovery document.
    pub fn discovery_document(&self) -> DiscoveryDocument {
        let strings = |values: &[&str]| values.iter().map(ToString::to_string).collect();
        DiscoveryDocument {
            issuer: self.issuer.clone(),
            jwks_uri: format!("{}{}", self.issuer, Self::JWKS_PATH),
            response_types_supported: strings(&["id_token"]),
            subject_types_supported: strings(&["public"]),
            id_token_signing_alg_values_supported: strings(&[TOKEN_ALGORITHM]),
            claims_supported: strings(&["iss", "sub", "aud", "exp", "iat", "jti"]),
        }
    }

    /// The JWK set holding the signer's public key.
    pub fn jwks(&self) -> JwkSet {
        JwkSet { keys: vec![self.signer.public_jwk()] }
    }

    /// Issues a token whose subject is `agent`'s [`AgentId`], addressed to `audience`.
    ///
    /// # Errors
    ///
    /// Returns [`FederationError::InvalidConfig`] when `audience` is empty and
    /// [`FederationError::Signing`] when the signer fails or no randomness is available.
    pub async fn issue(
        &self,
        agent: &AgentPrincipal,
        audience: &str,
    ) -> Result<AgentToken, FederationError> {
        if audience.is_empty() {
            return Err(FederationError::InvalidConfig {
                reason: "the token audience is empty; name the party the token is presented to"
                    .into(),
            });
        }
        let mut random = [0u8; 16];
        aws_lc_rs::rand::fill(&mut random)
            .map_err(|_| FederationError::Signing { reason: "no secure randomness".into() })?;
        // RFC 9562 §5.4: version 4, variant 10.
        random[6] = (random[6] & 0x0f) | 0x40;
        random[8] = (random[8] & 0x3f) | 0x80;
        let hex = hex::encode(random);
        let jti = format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        );

        let issued_at = Utc::now();
        let expires_at = issued_at + self.token_ttl;
        let claims = json!({
            "iss": self.issuer,
            "sub": agent.id.as_str(),
            "aud": audience,
            "iat": issued_at.timestamp(),
            "nbf": issued_at.timestamp() - NOT_BEFORE_LEEWAY,
            "exp": expires_at.timestamp(),
            "jti": jti,
        });
        let jws = self.sign_claims(&claims).await?;
        tracing::debug!(
            agent.id = %agent.id,
            token.audience = audience,
            token.jti = %jti,
            "agent token issued"
        );
        Ok(AgentToken::new(agent.id.clone(), jws, audience, expires_at))
    }

    /// The compact JWS of `claims` under the signer's key.
    pub(crate) async fn sign_claims(
        &self,
        claims: &serde_json::Value,
    ) -> Result<String, FederationError> {
        let header = json!({ "alg": TOKEN_ALGORITHM, "typ": "JWT", "kid": self.signer.kid() });
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = self.signer.sign(signing_input.as_bytes()).await?;
        Ok(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature)))
    }
}

#[async_trait]
impl AgentTokenSource for AgentTokenIssuer {
    async fn agent_token(
        &self,
        agent: &AgentPrincipal,
        audience: &str,
    ) -> Result<AgentToken, FederationError> {
        self.issue(agent, audience).await
    }
}
