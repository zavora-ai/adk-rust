//! Trust level assignment from request headers.

use async_trait::async_trait;
use awp_types::TrustLevel;
use axum::http::HeaderMap;

/// Trait for assigning a [`TrustLevel`] based on request headers.
///
/// Implementations can integrate with authentication systems (e.g. `adk-auth`)
/// to validate tokens and extract scopes.
#[async_trait]
pub trait TrustLevelAssigner: Send + Sync {
    /// Determine the trust level for a request based on its headers.
    async fn assign(&self, headers: &HeaderMap) -> TrustLevel;
}

/// Fail-closed default trust assigner.
///
/// Every request is anonymous because the presence of an `Authorization`
/// header does not prove that its credential is valid. Install a custom
/// implementation backed by `adk-auth` or another verifier before assigning
/// `Known`, `Partner`, or `Internal` trust.
pub struct DefaultTrustAssigner;

#[async_trait]
impl TrustLevelAssigner for DefaultTrustAssigner {
    async fn assign(&self, _headers: &HeaderMap) -> TrustLevel {
        TrustLevel::Anonymous
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_no_auth_is_anonymous() {
        let assigner = DefaultTrustAssigner;
        let headers = HeaderMap::new();
        assert_eq!(assigner.assign(&headers).await, TrustLevel::Anonymous);
    }

    #[tokio::test]
    async fn test_unverified_bearer_token_is_anonymous() {
        let assigner = DefaultTrustAssigner;
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Bearer some-token-here".parse().unwrap());
        assert_eq!(assigner.assign(&headers).await, TrustLevel::Anonymous);
    }

    #[tokio::test]
    async fn test_unverified_api_key_is_anonymous() {
        let assigner = DefaultTrustAssigner;
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "ApiKey my-api-key".parse().unwrap());
        assert_eq!(assigner.assign(&headers).await, TrustLevel::Anonymous);
    }

    #[tokio::test]
    async fn test_invalid_auth_scheme_is_anonymous() {
        let assigner = DefaultTrustAssigner;
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Basic dXNlcjpwYXNz".parse().unwrap());
        assert_eq!(assigner.assign(&headers).await, TrustLevel::Anonymous);
    }

    #[tokio::test]
    async fn test_empty_auth_header_is_anonymous() {
        let assigner = DefaultTrustAssigner;
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "".parse().unwrap());
        assert_eq!(assigner.assign(&headers).await, TrustLevel::Anonymous);
    }
}
