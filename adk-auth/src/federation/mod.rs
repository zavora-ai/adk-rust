//! Agent identity federation: an agent proves who it is to a cloud with a token its own
//! issuer signs, and a cloud's agent proves who it is to this service.
//!
//! | Direction | Item | Does |
//! |-----------|------|------|
//! | Issuer | [`AgentTokenIssuer`] | Publishes an OpenID Connect discovery document and JWK set, and signs short-lived RS256 tokens whose `sub` is the agent's [`AgentId`](adk_core::AgentId) |
//! | Issuer | [`AgentTokenSigner`], [`RsaTokenSigner`] | The signing key: a key service behind the trait, or an RSA key in memory |
//! | Outbound | [`AwsWebIdentity`] | `sts:AssumeRoleWithWebIdentity`: temporary credentials of an IAM role |
//! | Outbound | [`AgentCoreIdentity`] | `bedrock-agentcore:GetWorkloadAccessTokenForJWT`, signed with Signature Version 4 |
//! | Outbound | [`GoogleWorkloadIdentity`] | Workload identity federation: a Google Cloud access token |
//! | Outbound | [`EntraAgentIdentity`] | Microsoft Entra Agent ID: an access token of the agent identity |
//! | Inbound | [`InboundAgentVerifier`] | Validates a cloud's agent token and names the agent as an [`AgentPrincipal`](adk_core::AgentPrincipal) |
//! | Inbound | [`AgentClaimsProfile`], [`EntraAgentProfile`], [`GoogleAgentProfile`] | The mapping from validated claims to a principal, one per identity system |
//!
//! Each cloud is told once to trust the issuer URL and the agent's id as the subject. It
//! then fetches the issuer's keys itself, so no secret of the cloud is stored with the
//! agent. [`GoogleWorkloadIdentity`] and [`EntraAgentIdentity`] are also
//! [`CredentialBroker`](adk_core::CredentialBroker)s.
//!
//! | Cloud | Audience of the agent's token |
//! |-------|-------------------------------|
//! | AWS IAM | `sts.amazonaws.com` |
//! | Google Cloud | `https://iam.googleapis.com/projects/{number}/locations/global/workloadIdentityPools/{pool}/providers/{provider}` |
//! | Microsoft Entra | `api://AzureADTokenExchange` |
//!
//! # Example
//!
//! ```rust
//! use std::sync::Arc;
//!
//! use adk_auth::federation::{AgentTokenIssuer, GoogleWorkloadIdentity, RsaTokenSigner};
//! use adk_core::{AgentId, AgentPrincipal};
//!
//! # tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(async {
//! let issuer = Arc::new(AgentTokenIssuer::new(
//!     "https://agents.example.com",
//!     Arc::new(RsaTokenSigner::generate("agents-2026-11")?),
//! )?);
//! // Serve these two documents below the issuer URL.
//! let discovery = serde_json::to_string(&issuer.discovery_document())?;
//! let jwks = serde_json::to_string(&issuer.jwks())?;
//! assert!(discovery.contains("https://agents.example.com/jwks.json"));
//! assert!(jwks.contains("agents-2026-11"));
//!
//! let cashier = AgentPrincipal::new(AgentId::new("did:web:shop.example.com:agents:cashier")?);
//! let google =
//!     GoogleWorkloadIdentity::new("123456789012", "agents-pool", "agents-issuer", issuer.clone());
//! let token = issuer.issue(&cashier, &google.token_audience()).await?;
//! assert_eq!(token.subject, cashier.id);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! # }).unwrap();
//! ```

mod aws;
mod entra;
mod error;
mod exchange;
mod google;
mod inbound;
mod issuer;
mod sigv4;

pub use aws::{AgentCoreIdentity, AwsSessionCredentials, AwsWebIdentity, WorkloadAccessToken};
pub use entra::{EntraAccessToken, EntraAgentIdentity};
pub use error::{CloudProvider, FederationError};
pub use google::{GoogleAccessToken, GoogleWorkloadIdentity};
pub use inbound::{
    AgentClaimsProfile, EntraAgentProfile, GoogleAgentProfile, InboundAgentVerifier,
};
pub use issuer::{
    AgentToken, AgentTokenIssuer, AgentTokenSigner, AgentTokenSource, DiscoveryDocument, JwkSet,
    RsaPublicJwk, RsaTokenSigner, TOKEN_ALGORITHM,
};
