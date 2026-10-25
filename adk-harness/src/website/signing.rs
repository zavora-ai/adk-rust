//! Outbound Web Bot Auth: the site signs its own crawls, verifier requests and, when the
//! operator opts in, `web_fetch` calls (spec 3.5 §7.3 "Outbound").

use std::sync::Arc;

use adk_auth::web_bot_auth::{Ed25519SigningKey, HttpSignatureSigner, KeyDirectory};
use adk_core::{AdkError, Result};

use crate::web::RequestSigner;

/// A [`RequestSigner`] over `adk-auth`'s Web Bot Auth signer: every request, every redirect
/// hop included, carries `Signature-Agent`, `Signature-Input` and `Signature` (spec 3.5 §7.3
/// "Outbound").
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_auth::web_bot_auth::{Ed25519SigningKey, HttpSignatureSigner};
/// use adk_harness::web::RequestSigner;
/// use adk_harness::website::WebBotAuthSigner;
///
/// let key = Arc::new(Ed25519SigningKey::generate().unwrap());
/// let signer = WebBotAuthSigner::new(HttpSignatureSigner::builder(key, "https://bakery.example").build().unwrap());
/// let mut request = reqwest::Client::new().get("https://supplier.example/prices").build().unwrap();
/// signer.sign(&mut request).unwrap();
/// assert_eq!(request.headers()["signature-agent"], "sig1=\"https://bakery.example\"");
/// ```
#[derive(Debug)]
pub struct WebBotAuthSigner {
    inner: HttpSignatureSigner,
}

impl WebBotAuthSigner {
    /// Signs with `inner`.
    pub fn new(inner: HttpSignatureSigner) -> Self {
        Self { inner }
    }
}

impl RequestSigner for WebBotAuthSigner {
    fn sign(&self, request: &mut reqwest::Request) -> std::result::Result<(), String> {
        self.inner.sign_reqwest(request).map_err(|error| error.to_string())
    }
}

/// Signs the requests of `adk-awp`'s verifier, so `awp verify` identifies the site
/// (spec 3.5 §7.3 "Outbound").
#[async_trait::async_trait]
impl adk_awp::verify::fetch::RequestSigner for WebBotAuthSigner {
    async fn sign(
        &self,
        url: &str,
        headers: &mut reqwest::header::HeaderMap,
    ) -> std::result::Result<(), String> {
        let url = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
        let mut request = reqwest::Request::new(reqwest::Method::GET, url);
        *request.headers_mut() = headers.clone();
        self.inner.sign_reqwest(&mut request).map_err(|error| error.to_string())?;
        *headers = request.headers().clone();
        Ok(())
    }
}

/// The site's Web Bot Auth identity (spec 3.5 §7.3 "Outbound"): its signing key and the
/// origin whose `/.well-known/http-message-signatures-directory` publishes it.
///
/// The key never reaches a model context, a tool result or a log (A-I5): `Debug` names only
/// its thumbprint, and persistence goes through `Ed25519SigningKey::to_private_jwk`.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use adk_auth::web_bot_auth::Ed25519SigningKey;
/// use adk_harness::website::SiteSigning;
///
/// let signing = SiteSigning::new(Arc::new(Ed25519SigningKey::generate().unwrap()), "https://bakery.example").unwrap();
/// assert!(signing.directory_json().contains("\"kty\":\"OKP\""));
/// ```
#[derive(Clone)]
pub struct SiteSigning {
    key: Arc<Ed25519SigningKey>,
    origin: String,
    signer: Arc<WebBotAuthSigner>,
}

impl std::fmt::Debug for SiteSigning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteSigning")
            .field("origin", &self.origin)
            .field("keyid", &self.key.thumbprint())
            .finish()
    }
}

impl SiteSigning {
    /// The identity of `key`, published at `origin`.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when `origin` is not an origin the signer accepts.
    pub fn new(key: Arc<Ed25519SigningKey>, origin: impl Into<String>) -> Result<Self> {
        let origin = origin.into();
        let signer = HttpSignatureSigner::builder(key.clone(), origin.clone())
            .build()
            .map_err(|error| AdkError::config(format!("web bot auth signer: {error}")))?;
        Ok(Self { key, origin, signer: Arc::new(WebBotAuthSigner::new(signer)) })
    }

    /// The identity of the key a site persisted as a private JWK.
    ///
    /// # Errors
    ///
    /// As [`new`](Self::new), and when `jwk` is not an Ed25519 private JWK.
    pub fn from_private_jwk(jwk: &str, origin: impl Into<String>) -> Result<Self> {
        let key = Ed25519SigningKey::from_jwk(jwk)
            .map_err(|error| AdkError::config(format!("web bot auth key: {error}")))?;
        Self::new(Arc::new(key), origin)
    }

    /// The origin that publishes the key directory.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// The key's JWK thumbprint, sent as `keyid`.
    pub fn keyid(&self) -> &str {
        self.key.thumbprint()
    }

    /// The signer for the harness's fetches: pass it to
    /// [`HarnessAgentBuilder::web_signer`](crate::HarnessAgentBuilder::web_signer) to sign
    /// `web_fetch`, and to the site runtime and verifier for crawls and `awp verify`.
    pub fn request_signer(&self) -> Arc<dyn RequestSigner> {
        self.signer.clone()
    }

    /// The signer for `adk-awp`'s verifier: pass it to `HttpFetcher::with_signer`.
    pub fn verifier_signer(&self) -> Arc<dyn adk_awp::verify::fetch::RequestSigner> {
        self.signer.clone()
    }

    /// The key directory the site publishes at
    /// `/.well-known/http-message-signatures-directory`.
    pub fn directory(&self) -> KeyDirectory {
        KeyDirectory::from_keys([self.key.as_ref()])
    }

    /// [`directory`](Self::directory) as JSON.
    pub fn directory_json(&self) -> String {
        self.directory().to_json()
    }
}
