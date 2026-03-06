//! Token claims extracted from validated JWTs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Standard claims from a validated JWT.
///
/// Contains both standard OIDC claims and provider-specific custom claims.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenClaims {
    /// Subject (user identifier).
    pub sub: String,

    /// Issuer URL.
    #[serde(default)]
    pub iss: String,

    /// Audience (client IDs this token is intended for).
    #[serde(default)]
    pub aud: Audience,

    /// Expiration time (Unix timestamp).
    #[serde(default)]
    pub exp: u64,

    /// Issued at time (Unix timestamp).
    #[serde(default)]
    pub iat: u64,

    /// Not before time (Unix timestamp).
    #[serde(default)]
    pub nbf: Option<u64>,

    /// JWT ID (unique identifier for this token).
    #[serde(default)]
    pub jti: Option<String>,

    /// Email address (if available).
    #[serde(default)]
    pub email: Option<String>,

    /// Whether email is verified.
    #[serde(default)]
    pub email_verified: Option<bool>,

    /// User's full name.
    #[serde(default)]
    pub name: Option<String>,

    /// User's given (first) name.
    #[serde(default)]
    pub given_name: Option<String>,

    /// User's family (last) name.
    #[serde(default)]
    pub family_name: Option<String>,

    /// User's preferred username.
    #[serde(default)]
    pub preferred_username: Option<String>,

    /// User's picture URL.
    #[serde(default)]
    pub picture: Option<String>,

    /// Groups/roles from the identity provider.
    #[serde(default)]
    pub groups: Vec<String>,

    /// Roles (some providers use this instead of groups).
    #[serde(default)]
    pub roles: Vec<String>,

    /// Azure AD specific: tenant ID.
    #[serde(default)]
    pub tid: Option<String>,

    /// Google specific: hosted domain.
    #[serde(default)]
    pub hd: Option<String>,

    /// Custom claims (provider-specific).
    #[serde(flatten)]
    pub custom: HashMap<String, serde_json::Value>,
}

impl TokenClaims {
    /// Get the user identifier, preferring email over sub.
    pub fn user_id(&self) -> &str {
        self.email.as_deref().unwrap_or(&self.sub)
    }

    /// Get all groups and roles combined.
    pub fn all_groups(&self) -> Vec<&str> {
        self.groups.iter().chain(self.roles.iter()).map(|s| s.as_str()).collect()
    }

    /// Check if token is expired.
    pub fn is_expired(&self) -> bool {
        let now =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        self.exp < now
    }

    /// Get a custom claim by key.
    pub fn get_custom<T: serde::de::DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.custom.get(key).and_then(|v| serde_json::from_value(v.clone()).ok())
    }
}

/// Audience can be a single string or array of strings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(untagged)]
pub enum Audience {
    #[default]
    None,
    Single(String),
    Multiple(Vec<String>),
}

impl Audience {
    /// Check if audience contains a specific value.
    pub fn contains(&self, value: &str) -> bool {
        match self {
            Audience::None => false,
            Audience::Single(s) => s == value,
            Audience::Multiple(v) => v.iter().any(|s| s == value),
        }
    }

    /// Get all audiences as a vector.
    pub fn as_vec(&self) -> Vec<&str> {
        match self {
            Audience::None => vec![],
            Audience::Single(s) => vec![s.as_str()],
            Audience::Multiple(v) => v.iter().map(|s| s.as_str()).collect(),
        }
    }
}

impl Default for TokenClaims {
    fn default() -> Self {
        Self {
            sub: String::new(),
            iss: String::new(),
            aud: Audience::None,
            exp: 0,
            iat: 0,
            nbf: None,
            jti: None,
            email: None,
            email_verified: None,
            name: None,
            given_name: None,
            family_name: None,
            preferred_username: None,
            picture: None,
            groups: Vec::new(),
            roles: Vec::new(),
            tid: None,
            hd: None,
            custom: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_claims_user_id() {
        let claims = TokenClaims {
            sub: "user-123".to_string(),
            email: Some("alice@example.com".to_string()),
            ..Default::default()
        };
        assert_eq!(claims.user_id(), "alice@example.com");

        let claims_no_email =
            TokenClaims { sub: "user-123".to_string(), email: None, ..Default::default() };
        assert_eq!(claims_no_email.user_id(), "user-123");
    }

    #[test]
    fn test_audience_contains() {
        let single = Audience::Single("client-1".to_string());
        assert!(single.contains("client-1"));
        assert!(!single.contains("client-2"));

        let multiple = Audience::Multiple(vec!["client-1".to_string(), "client-2".to_string()]);
        assert!(multiple.contains("client-1"));
        assert!(multiple.contains("client-2"));
        assert!(!multiple.contains("client-3"));
    }
}
