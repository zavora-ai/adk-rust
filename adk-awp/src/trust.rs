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

/// Default trust assigner that checks for `Authorization` headers.
///
/// - No credentials → [`TrustLevel::Anonymous`]
/// - `Bearer` or `ApiKey` token present → [`TrustLevel::Known`]
///
/// Replace with `adk-auth` integration for full JWT scope extraction
/// (Partner/Internal levels) when available.
pub struct DefaultTrustAssigner;

#[async_trait]
impl TrustLevelAssigner for DefaultTrustAssigner {
    async fn assign(&self, headers: &HeaderMap) -> TrustLevel {
        if let Some(auth) = headers.get("Authorization") {
            if let Ok(val) = auth.to_str() {
                if val.starts_with("Bearer ") || val.starts_with("ApiKey ") {
                    return TrustLevel::Known;
                }
            }
        }
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
    async fn test_bearer_token_is_known() {
        let assigner = DefaultTrustAssigner;
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "Bearer some-token-here".parse().unwrap());
        assert_eq!(assigner.assign(&headers).await, TrustLevel::Known);
    }

    #[tokio::test]
    async fn test_api_key_is_known() {
        let assigner = DefaultTrustAssigner;
        let mut headers = HeaderMap::new();
        headers.insert("Authorization", "ApiKey my-api-key".parse().unwrap());
        assert_eq!(assigner.assign(&headers).await, TrustLevel::Known);
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
