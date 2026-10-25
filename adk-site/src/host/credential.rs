//! Credentials host adapters present, minted per request by a credential broker (spec 3.5
//! §7.8 "Hosts", A-I19).

use std::fmt;
use std::sync::Arc;

use adk_core::{ActingContext, CredentialBroker, CredentialSecret, Url};

use crate::error::{Result, SiteError};

/// The credential a host adapter presents to its provider, minted by a
/// [`CredentialBroker`] for the acting agent at the provider's resource (spec 3.5 §7.8
/// "Hosts", A-I19).
///
/// The adapter asks the broker for a token on every request and checks, with
/// [`ScopedToken::authorize_for`](adk_core::ScopedToken::authorize_for), that the request
/// falls under the token's audience, so a secret only ever reaches the endpoint it was
/// bound to. No credential passes through a model context or a file the adapter writes,
/// except the SSH identity `RsyncHost` hands to `ssh` in a
/// private temporary file.
///
/// What the secret holds depends on the adapter:
///
/// | Adapter | Secret |
/// |---------|--------|
/// | S3 and R2 | JSON `{"accessKeyId", "secretAccessKey", "sessionToken"?}` |
/// | Google Cloud Storage | An OAuth 2.0 access token |
/// | Azure Blob Storage | The account key, a bearer token or a SAS query string, per `AzureAuth` |
/// | Cloudflare cache purge, Netlify | An API token |
/// | Git pages branch | A token sent as HTTP basic auth (`x-access-token`) |
/// | rsync over SSH | An OpenSSH private key |
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use adk_core::{
///     ActingContext, CredentialBroker, Result, RevocationScope, ScopedToken, SubjectKind,
///     async_trait, resource_indicator,
/// };
/// use adk_site::host::HostCredential;
///
/// #[derive(Debug)]
/// struct Vault;
///
/// #[async_trait]
/// impl CredentialBroker for Vault {
///     async fn token_for(
///         &self,
///         acting: &ActingContext,
///         resource: &adk_core::Url,
///         scopes: &[String],
///     ) -> Result<ScopedToken> {
///         Ok(ScopedToken::new(acting.agent.id.clone(), "token", resource.clone(), scopes.to_vec()))
///     }
///
///     async fn revoke(&self, _scope: &RevocationScope) -> Result<u64> {
///         Ok(0)
///     }
/// }
///
/// let acting = ActingContext::derived("site", "publisher", "owner", SubjectKind::Human);
/// let resource = resource_indicator("https://api.netlify.com/api/v1/")?;
/// let credential = HostCredential::new(Arc::new(Vault), acting, resource);
/// assert_eq!(credential.resource().host_str(), Some("api.netlify.com"));
/// # Ok::<(), adk_core::AdkError>(())
/// ```
#[derive(Clone)]
pub struct HostCredential {
    broker: Arc<dyn CredentialBroker>,
    acting: ActingContext,
    resource: Url,
    scopes: Vec<String>,
}

impl fmt::Debug for HostCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostCredential")
            .field("agent", &self.acting.agent.id)
            .field("resource", &self.resource.as_str())
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl HostCredential {
    /// A credential `broker` mints for `acting` at `resource`.
    pub fn new(broker: Arc<dyn CredentialBroker>, acting: ActingContext, resource: Url) -> Self {
        Self { broker, acting, resource, scopes: Vec::new() }
    }

    /// Asks the broker for `scopes` with each token.
    #[must_use]
    pub fn with_scopes(mut self, scopes: Vec<String>) -> Self {
        self.scopes = scopes;
        self
    }

    /// The resource the broker binds the credential to.
    pub fn resource(&self) -> &Url {
        &self.resource
    }

    /// The secret for a request to `request`.
    ///
    /// # Errors
    ///
    /// [`SiteError::Host`] when the broker refuses or `request` is outside the token's
    /// audience.
    pub(crate) async fn secret_for(&self, host: &str, request: &Url) -> Result<CredentialSecret> {
        let token =
            self.broker.token_for(&self.acting, &self.resource, &self.scopes).await.map_err(
                |e| SiteError::host(host, format!("the credential broker refused: {e}")),
            )?;
        let secret = token
            .authorize_for(request)
            .map_err(|e| SiteError::host(host, format!("credential not usable here: {e}")))?;
        Ok(CredentialSecret::new(secret.expose()))
    }
}
