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
    /// Get the user identifier, preferring a verified email over sub.
    pub fn user_id(&self) -> &str {
        self.verified_email().unwrap_or(&self.sub)
    }

    /// Returns true when the token contains a verified email address.
    pub fn email_is_verified(&self) -> bool {
        self.email_verified.unwrap_or(false) && self.email.as_deref().is_some()
    }

    /// Get the verified email address if available.
    pub fn verified_email(&self) -> Option<&str> {
        self.email_is_verified().then_some(self.email.as_deref()).flatten()
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

    /// Get granted scopes from the standard `scope` or `scp` claims.
    pub fn scopes(&self) -> Vec<String> {
        let mut scopes = Vec::new();
        self.extend_scopes(&mut scopes, "scope");
        self.extend_scopes(&mut scopes, "scp");
        scopes
    }

    fn extend_scopes(&self, scopes: &mut Vec<String>, claim: &str) {
        let Some(value) = self.custom.get(claim) else {
            return;
        };

        match value {
            serde_json::Value::String(scope_string) => {
                for scope in scope_string.split_whitespace() {
                    if !scopes.iter().any(|existing| existing == scope) {
                        scopes.push(scope.to_string());
                    }
                }
            }
            serde_json::Value::Array(values) => {
                for scope in values.iter().filter_map(serde_json::Value::as_str) {
                    if !scopes.iter().any(|existing| existing == scope) {
                        scopes.push(scope.to_string());
                    }
                }
            }
            _ => {}
        }
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
            sub: "user-123".into(),
            email: Some("alice@example.com".into()),
            email_verified: Some(true),
            ..Default::default()
        };
        assert_eq!(claims.user_id(), "alice@example.com");

        let claims_no_email =
            TokenClaims { sub: "user-123".into(), email: None, ..Default::default() };
        assert_eq!(claims_no_email.user_id(), "user-123");
    }

    #[test]
    fn test_token_claims_unverified_email_falls_back_to_sub() {
        let claims = TokenClaims {
            sub: "user-123".into(),
            email: Some("alice@example.com".into()),
            email_verified: Some(false),
            ..Default::default()
        };

        assert_eq!(claims.user_id(), "user-123");
        assert_eq!(claims.verified_email(), None);
    }

    #[test]
    fn test_token_claims_scopes_from_scope_and_scp() {
        let mut claims = TokenClaims::default();
        claims.custom.insert("scope".into(), serde_json::json!("read write"));
        claims.custom.insert("scp".into(), serde_json::json!(["write", "admin"]));

        assert_eq!(claims.scopes(), vec!["read", "write", "admin"]);
    }

    #[test]
    fn test_audience_contains() {
        let single = Audience::Single("client-1".into());
        assert!(single.contains("client-1"));
        assert!(!single.contains("client-2"));

        let multiple = Audience::Multiple(vec!["client-1".into(), "client-2".into()]);
        assert!(multiple.contains("client-1"));
        assert!(multiple.contains("client-2"));
        assert!(!multiple.contains("client-3"));
    }
}
