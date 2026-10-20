//! SSO access control integration.

use super::{ClaimsMapper, TokenClaims, TokenError, TokenValidator};
use crate::{AccessControl, AuditEvent, AuditFailureMode, AuditOutcome, AuditSink, Permission};
use std::sync::Arc;

/// Combines SSO token validation with adk-auth access control.
///
/// With an audit sink, a grant whose decision the sink fails to record is refused with
/// [`SsoError::AuditFailed`] ([`AuditFailureMode::Block`], the default), as
/// [`ProtectedTool`](crate::ProtectedTool) and [`ScopeGuard`](crate::ScopeGuard) refuse
/// the call; see [`SsoAccessControlBuilder::audit_failure_mode`].
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
    audit_failure_mode: AuditFailureMode,
}

impl SsoAccessControl {
    /// Create a new builder.
    pub fn builder() -> SsoAccessControlBuilder {
        SsoAccessControlBuilder::default()
    }

    /// Validate token and check permission.
    ///
    /// Returns the token claims if access is granted. With an audit sink, the decision is
    /// recorded before it is returned.
    ///
    /// # Errors
    ///
    /// Returns [`SsoError::TokenError`] when the token does not validate,
    /// [`SsoError::AccessDenied`] when no mapped role grants `permission`, and
    /// [`SsoError::AuditFailed`] when access would be granted but the audit sink failed to
    /// record it under [`AuditFailureMode::Block`].
    pub async fn check_token(
        &self,
        token: &str,
        permission: &Permission,
    ) -> Result<TokenClaims, SsoError> {
        let claims = self.validator.validate(token).await?;
        let user_id = self.mapper.get_user_id(&claims);
        let roles = self.mapper.map_to_roles(&claims);
        let result = self.check_with_roles(&user_id, &roles, permission);

        let recorded = match &self.audit_sink {
            Some(sink) => {
                let outcome =
                    if result.is_ok() { AuditOutcome::Allowed } else { AuditOutcome::Denied };
                let event = AuditEvent::tool_access(&user_id, &permission.to_string(), outcome);
                self.record(sink.as_ref(), event, &user_id, permission).await
            }
            None => Ok(()),
        };

        // A denial stays a denial when its record also failed.
        result?;
        recorded?;
        Ok(claims)
    }

    /// Records one decision, refusing access when the sink fails in `Block` mode.
    async fn record(
        &self,
        sink: &dyn AuditSink,
        event: AuditEvent,
        user_id: &str,
        permission: &Permission,
    ) -> Result<(), SsoError> {
        let Err(error) = sink.log(event).await else {
            return Ok(());
        };
        match self.audit_failure_mode {
            AuditFailureMode::Block => {
                tracing::error!(
                    user.id = %user_id,
                    permission = %permission,
                    error = %error,
                    "audit sink failed to record the access decision; access is not granted"
                );
                Err(SsoError::AuditFailed {
                    user: user_id.to_string(),
                    permission: permission.to_string(),
                    reason: error.to_string(),
                })
            }
            AuditFailureMode::Warn => {
                tracing::warn!(
                    user.id = %user_id,
                    permission = %permission,
                    error = %error,
                    "audit sink failed; continuing without a record"
                );
                Ok(())
            }
        }
    }

    /// Check permission with pre-mapped roles.
    fn check_with_roles(
        &self,
        user_id: &str,
        roles: &[String],
        permission: &Permission,
    ) -> Result<(), SsoError> {
        if self.access_control.check_roles(roles, permission) {
            Ok(())
        } else {
            Err(SsoError::AccessDenied {
                user: user_id.to_string(),
                permission: permission.to_string(),
            })
        }
    }

    /// Get the underlying access control.
    pub fn access_control(&self) -> &AccessControl {
        &self.access_control
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Role;
    use async_trait::async_trait;

    struct DummyValidator;

    #[async_trait]
    impl TokenValidator for DummyValidator {
        async fn validate(&self, _token: &str) -> Result<TokenClaims, TokenError> {
            Err(TokenError::ValidationError("not used in unit test".into()))
        }

        fn issuer(&self) -> &str {
            "test-issuer"
        }
    }

    #[test]
    fn test_check_with_roles_honors_deny_precedence() {
        let access_control = AccessControl::builder()
            .role(Role::new("editor").allow(Permission::AllTools))
            .role(Role::new("restricted").deny(Permission::Tool("code_exec".into())))
            .build()
            .unwrap();

        let sso = SsoAccessControl {
            validator: Arc::new(DummyValidator),
            mapper: ClaimsMapper::builder().build(),
            access_control,
            audit_sink: None,
            audit_failure_mode: AuditFailureMode::default(),
        };

        assert!(
            sso.check_with_roles(
                "bob",
                &["editor".to_string(), "restricted".to_string()],
                &Permission::Tool("code_exec".into())
            )
            .is_err()
        );
        assert!(
            sso.check_with_roles(
                "bob",
                &["restricted".to_string(), "editor".to_string()],
                &Permission::Tool("code_exec".into())
            )
            .is_err()
        );
        assert!(
            sso.check_with_roles(
                "bob",
                &["editor".to_string(), "restricted".to_string()],
                &Permission::Tool("search".into())
            )
            .is_ok()
        );
    }

    /// Accepts every token as `alice` in the `Engineering` group.
    struct AcceptingValidator;

    #[async_trait]
    impl TokenValidator for AcceptingValidator {
        async fn validate(&self, _token: &str) -> Result<TokenClaims, TokenError> {
            Ok(TokenClaims {
                sub: "alice".to_string(),
                groups: vec!["Engineering".to_string()],
                ..TokenClaims::default()
            })
        }

        fn issuer(&self) -> &str {
            "test-issuer"
        }
    }

    /// A sink whose store is down.
    struct FailingSink;

    #[async_trait]
    impl AuditSink for FailingSink {
        async fn log(&self, _event: AuditEvent) -> Result<(), crate::AuthError> {
            Err(crate::AuthError::AuditError("audit store unavailable".into()))
        }
    }

    fn engineers_deploy(sink: impl AuditSink + 'static) -> SsoAccessControlBuilder {
        SsoAccessControl::builder()
            .validator(AcceptingValidator)
            .mapper(ClaimsMapper::builder().map_group("Engineering", "engineer").build())
            .access_control(
                AccessControl::builder()
                    .role(Role::new("engineer").allow(Permission::Tool("deploy".into())))
                    .build()
                    .unwrap(),
            )
            .audit_sink(sink)
    }

    #[tokio::test]
    async fn a_grant_the_sink_cannot_record_is_refused_by_default() {
        let sso = engineers_deploy(FailingSink).build().unwrap();
        let error = sso.check_token("token", &Permission::Tool("deploy".into())).await.unwrap_err();
        assert!(
            matches!(&error, SsoError::AuditFailed { user, permission, .. }
                if user == "alice" && permission == "tool:deploy"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn warn_mode_grants_access_without_a_record() {
        let sso = engineers_deploy(FailingSink)
            .audit_failure_mode(AuditFailureMode::Warn)
            .build()
            .unwrap();
        let claims = sso.check_token("token", &Permission::Tool("deploy".into())).await.unwrap();
        assert_eq!(claims.sub, "alice");
    }

    #[tokio::test]
    async fn a_denial_stays_a_denial_when_its_record_fails() {
        let sso = engineers_deploy(FailingSink).build().unwrap();
        let error =
            sso.check_token("token", &Permission::Tool("drop_database".into())).await.unwrap_err();
        assert!(matches!(error, SsoError::AccessDenied { .. }), "{error}");
    }

    #[tokio::test]
    async fn a_recorded_grant_is_returned() {
        let sink = Arc::new(crate::InMemoryAuditSink::new());
        let sso = engineers_deploy(sink.clone()).build().unwrap();
        let claims = sso.check_token("token", &Permission::Tool("deploy".into())).await.unwrap();
        assert_eq!(claims.sub, "alice");
        assert_eq!(sink.len().await, 1);
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

    /// Access would be granted, but the audit sink failed to record the decision.
    #[error(
        "access for user '{user}' to '{permission}' refused: the decision could not be audited \
         ({reason}). Restore the audit sink, or set AuditFailureMode::Warn to grant access \
         without a record"
    )]
    AuditFailed { user: String, permission: String, reason: String },
}

/// Builder for SsoAccessControl.
#[derive(Default)]
pub struct SsoAccessControlBuilder {
    validator: Option<Arc<dyn TokenValidator>>,
    mapper: Option<ClaimsMapper>,
    access_control: Option<AccessControl>,
    audit_sink: Option<Arc<dyn AuditSink>>,
    audit_failure_mode: AuditFailureMode,
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
    ///
    /// A grant whose decision `sink` fails to record is refused; use
    /// [`audit_failure_mode`](Self::audit_failure_mode) to change that.
    pub fn audit_sink(mut self, sink: impl AuditSink + 'static) -> Self {
        self.audit_sink = Some(Arc::new(sink));
        self
    }

    /// Sets what happens when the audit sink fails to record a decision:
    /// [`AuditFailureMode::Block`] (the default) refuses the grant with
    /// [`SsoError::AuditFailed`], [`AuditFailureMode::Warn`] logs the failure and grants
    /// access without a record.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use adk_auth::AuditFailureMode;
    /// use adk_auth::sso::SsoAccessControl;
    ///
    /// let sso = SsoAccessControl::builder()
    ///     .validator(provider)
    ///     .access_control(ac)
    ///     .audit_sink(audit)
    ///     .audit_failure_mode(AuditFailureMode::Warn)
    ///     .build()?;
    /// ```
    pub fn audit_failure_mode(mut self, mode: AuditFailureMode) -> Self {
        self.audit_failure_mode = mode;
        self
    }

    /// Build the SsoAccessControl.
    pub fn build(self) -> Result<SsoAccessControl, &'static str> {
        Ok(SsoAccessControl {
            validator: self.validator.ok_or("validator is required")?,
            mapper: self.mapper.unwrap_or_else(|| ClaimsMapper::builder().build()),
            access_control: self.access_control.ok_or("access_control is required")?,
            audit_sink: self.audit_sink,
            audit_failure_mode: self.audit_failure_mode,
        })
    }
}
