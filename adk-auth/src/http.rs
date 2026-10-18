//! The HTTP client the identity-provider integrations use by default.

use std::time::Duration;

/// Longest wait for a TCP and TLS connection to an identity provider.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A client whose requests give up after `request_timeout`, connection included.
///
/// Without a deadline a provider that accepts the connection and never answers holds the
/// caller, and any lock it holds, indefinitely.
pub(crate) fn client(request_timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(request_timeout)
        .build()
        // Building fails only when the TLS backend cannot initialize, as `Client::new` does.
        .unwrap_or_default()
}
