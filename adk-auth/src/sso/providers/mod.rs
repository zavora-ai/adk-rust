//! SSO provider implementations.

mod oidc;

pub use oidc::OidcProvider;

// Provider-specific implementations
mod auth0;
mod azure;
mod google;
mod okta;

pub use auth0::Auth0Provider;
pub use azure::AzureADProvider;
pub use google::GoogleProvider;
pub use okta::OktaProvider;
