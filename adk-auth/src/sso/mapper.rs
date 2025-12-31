//! Claims mapping for SSO integration.

use super::TokenClaims;
use std::collections::HashMap;

/// Maps IdP claims to adk-auth roles.
///
/// # Example
///
/// ```rust,ignore
/// let mapper = ClaimsMapper::builder()
///     .map_group("AdminGroup", "admin")
///     .map_group("Users", "user")
///     .default_role("guest")
///     .build();
///
/// let roles = mapper.map_to_roles(&claims);
/// ```
pub struct ClaimsMapper {
    /// Map IdP groups to adk-auth roles.
    group_to_role: HashMap<String, String>,
    /// Default role for authenticated users without matching groups.
    default_role: Option<String>,
    /// Which claim to use as user ID.
    user_id_claim: UserIdClaim,
}

/// Which claim to use as user ID.
#[derive(Debug, Clone, Default)]
pub enum UserIdClaim {
    /// Use the 'sub' claim.
    #[default]
    Sub,
    /// Use the 'email' claim.
    Email,
    /// Use the 'preferred_username' claim.
    PreferredUsername,
    /// Use a custom claim.
    Custom(String),
}

impl ClaimsMapper {
    /// Create a new builder.
    pub fn builder() -> ClaimsMapperBuilder {
        ClaimsMapperBuilder::default()
    }

    /// Get user ID from claims based on configured claim.
    pub fn get_user_id(&self, claims: &TokenClaims) -> String {
        match &self.user_id_claim {
            UserIdClaim::Sub => claims.sub.clone(),
            UserIdClaim::Email => claims.email.clone().unwrap_or_else(|| claims.sub.clone()),
            UserIdClaim::PreferredUsername => claims
                .preferred_username
                .clone()
                .unwrap_or_else(|| claims.sub.clone()),
            UserIdClaim::Custom(key) => claims
                .get_custom::<String>(key)
                .unwrap_or_else(|| claims.sub.clone()),
        }
    }

    /// Map claims to adk-auth role names.
    pub fn map_to_roles(&self, claims: &TokenClaims) -> Vec<String> {
        let mut roles = Vec::new();

        // Check groups claim
        for group in &claims.groups {
            if let Some(role) = self.group_to_role.get(group) {
                if !roles.contains(role) {
                    roles.push(role.clone());
                }
            }
        }

        // Check roles claim (some providers use this)
        for role in &claims.roles {
            if let Some(mapped_role) = self.group_to_role.get(role) {
                if !roles.contains(mapped_role) {
                    roles.push(mapped_role.clone());
                }
            }
        }

        // Add default role if no roles matched
        if roles.is_empty() {
            if let Some(default) = &self.default_role {
                roles.push(default.clone());
            }
        }

        roles
    }
}

/// Builder for ClaimsMapper.
#[derive(Default)]
pub struct ClaimsMapperBuilder {
    group_to_role: HashMap<String, String>,
    default_role: Option<String>,
    user_id_claim: UserIdClaim,
}

impl ClaimsMapperBuilder {
    /// Map an IdP group to an adk-auth role.
    pub fn map_group(mut self, group: impl Into<String>, role: impl Into<String>) -> Self {
        self.group_to_role.insert(group.into(), role.into());
        self
    }

    /// Set the default role for users without matching groups.
    pub fn default_role(mut self, role: impl Into<String>) -> Self {
        self.default_role = Some(role.into());
        self
    }

    /// Use 'sub' claim as user ID.
    pub fn user_id_from_sub(mut self) -> Self {
        self.user_id_claim = UserIdClaim::Sub;
        self
    }

    /// Use 'email' claim as user ID.
    pub fn user_id_from_email(mut self) -> Self {
        self.user_id_claim = UserIdClaim::Email;
        self
    }

    /// Use 'preferred_username' claim as user ID.
    pub fn user_id_from_preferred_username(mut self) -> Self {
        self.user_id_claim = UserIdClaim::PreferredUsername;
        self
    }

    /// Use a custom claim as user ID.
    pub fn user_id_from_claim(mut self, claim: impl Into<String>) -> Self {
        self.user_id_claim = UserIdClaim::Custom(claim.into());
        self
    }

    /// Build the ClaimsMapper.
    pub fn build(self) -> ClaimsMapper {
        ClaimsMapper {
            group_to_role: self.group_to_role,
            default_role: self.default_role,
            user_id_claim: self.user_id_claim,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_claims() -> TokenClaims {
        TokenClaims {
            sub: "user-123".into(),
            email: Some("alice@example.com".into()),
            preferred_username: Some("alice".into()),
            groups: vec!["AdminGroup".into(), "Users".into()],
            ..Default::default()
        }
    }

    #[test]
    fn test_map_groups_to_roles() {
        let mapper = ClaimsMapper::builder()
            .map_group("AdminGroup", "admin")
            .map_group("Users", "user")
            .build();

        let roles = mapper.map_to_roles(&test_claims());
        assert!(roles.contains(&"admin".to_string()));
        assert!(roles.contains(&"user".to_string()));
    }

    #[test]
    fn test_default_role() {
        let mapper = ClaimsMapper::builder()
            .map_group("NonExistent", "special")
            .default_role("guest")
            .build();

        let roles = mapper.map_to_roles(&test_claims());
        assert_eq!(roles, vec!["guest".to_string()]);
    }

    #[test]
    fn test_user_id_from_email() {
        let mapper = ClaimsMapper::builder().user_id_from_email().build();
        assert_eq!(mapper.get_user_id(&test_claims()), "alice@example.com");
    }

    #[test]
    fn test_user_id_from_sub() {
        let mapper = ClaimsMapper::builder().user_id_from_sub().build();
        assert_eq!(mapper.get_user_id(&test_claims()), "user-123");
    }
}
