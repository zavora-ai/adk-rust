//! SSO access control integration.

use super::{ClaimsMapper, TokenClaims, TokenError, TokenValidator};
use crate::{AccessControl, AuditEvent, AuditOutcome, AuditSink, Permission};
use std::sync::Arc;

/// Combines SSO token validation with adk-auth access control.
///
/// # Example
///
/// ```rust,ignore
/// let sso = SsoAccessControl::builder()
///     .validator(GoogleProvider::new("client-id"))
///     .mapper(ClaimsMapper::builder()
///         .map_group("Admin", "admin")
///         .default_role("user")
///         .build())
///     .access_control(ac)
///     .build()?;
///
/// let claims = sso.check_token(token, &Permission::Tool("search".into())).await?;
/// ```
pub struct SsoAccessControl {
    validator: Arc<dyn TokenValidator>,
    mapper: ClaimsMapper,
    access_control: AccessControl,
    audit_sink: Option<Arc<dyn AuditSink>>,
}

impl SsoAccessControl {
    /// Create a new builder.
    pub fn builder() -> SsoAccessControlBuilder {
        SsoAccessControlBuilder::default()
    }

    /// Validate token and check permission.
    ///
    /// Returns the token claims if access is granted.
    pub async fn check_token(
        &self,
        token: &str,
        permission: &Permission,
    ) -> Result<TokenClaims, SsoError> {
        // Step 1: Validate the token
        let claims = self.validator.validate(token).await?;

        // Step 2: Get user ID from claims
        let user_id = self.mapper.get_user_id(&claims);

        // Step 3: Map claims to roles and check access
        let roles = self.mapper.map_to_roles(&claims);
        let result = self.check_with_roles(&user_id, &roles, permission);

        // Step 4: Audit log
        if let Some(sink) = &self.audit_sink {
            let outcome = if result.is_ok() {
                AuditOutcome::Allowed
            } else {
                AuditOutcome::Denied
            };
            let event = AuditEvent::tool_access(&user_id, &permission.to_string(), outcome);
            let _ = sink.log(event).await;
        }

        // Return result
        result?;
        Ok(claims)
    }

    /// Check permission with pre-mapped roles.
    fn check_with_roles(
        &self,
        user_id: &str,
        roles: &[String],
        permission: &Permission,
    ) -> Result<(), SsoError> {
        // Check if any of the user's roles grant the permission
        for role_name in roles {
            if let Some(role) = self.access_control.get_role(role_name) {
                if role.can_access(permission) {
                    return Ok(());
                }
            }
        }

        Err(SsoError::AccessDenied {
            user: user_id.to_string(),
            permission: permission.to_string(),
        })
    }

    /// Get the underlying access control.
    pub fn access_control(&self) -> &AccessControl {
        &self.access_control
    }
}

/// Errors from SSO access control.
#[derive(Debug, thiserror::Error)]
pub enum SsoError {
    /// Token validation failed.
    #[error("Token error: {0}")]
    TokenError(#[from] TokenError),

    /// Access denied.
    #[error("Access denied for user '{user}' to '{permission}'")]
    AccessDenied { user: String, permission: String },
}

/// Builder for SsoAccessControl.
#[derive(Default)]
pub struct SsoAccessControlBuilder {
    validator: Option<Arc<dyn TokenValidator>>,
    mapper: Option<ClaimsMapper>,
    access_control: Option<AccessControl>,
    audit_sink: Option<Arc<dyn AuditSink>>,
}

impl SsoAccessControlBuilder {
    /// Set the token validator.
    pub fn validator(mut self, v: impl TokenValidator + 'static) -> Self {
        self.validator = Some(Arc::new(v));
        self
    }

    /// Set the claims mapper.
    pub fn mapper(mut self, m: ClaimsMapper) -> Self {
        self.mapper = Some(m);
        self
    }

    /// Set the access control.
    pub fn access_control(mut self, ac: AccessControl) -> Self {
        self.access_control = Some(ac);
        self
    }

    /// Set the audit sink.
    pub fn audit_sink(mut self, sink: impl AuditSink + 'static) -> Self {
        self.audit_sink = Some(Arc::new(sink));
        self
    }

    /// Build the SsoAccessControl.
    pub fn build(self) -> Result<SsoAccessControl, &'static str> {
        Ok(SsoAccessControl {
            validator: self.validator.ok_or("validator is required")?,
            mapper: self.mapper.unwrap_or_else(|| ClaimsMapper::builder().build()),
            access_control: self.access_control.ok_or("access_control is required")?,
            audit_sink: self.audit_sink,
        })
    }
}
