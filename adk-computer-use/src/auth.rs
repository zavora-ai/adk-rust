//! Coarse `computer:*` scope authorization bound to an adk-auth identity.
//!
//! [`ScopeAuthorizer`] is a *gate*, not an approval: it confirms the caller
//! holds the entitlement for the requested [`ExecutionMode`] and that the
//! action's principal/tenant match the identity already verified by adk-auth.
//! The runtime policy engine still evaluates the exact action independently.

use crate::{ActionClass, ExecutionMode};
use adk_auth::check_scopes;
use thiserror::Error;

/// Authenticated identity and exact operation context forwarded to the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerUseAuthContext {
    /// Authenticated principal proposing the action.
    pub principal_id: String,
    /// Authenticated tenant, when multi-tenant.
    pub tenant_id: Option<String>,
    /// The session the action belongs to.
    pub session_id: String,
    /// Execution group for multi-agent coordination.
    pub execution_group_id: String,
    /// The execution mode being requested.
    pub requested_mode: ExecutionMode,
    /// The operation-aware action class.
    pub action_class: ActionClass,
    /// Target application/bundle identifier, when applicable.
    pub target_app: Option<String>,
    /// Target window identifier, when applicable.
    pub target_window: Option<String>,
    /// Digest of the policy under which the action is proposed.
    pub policy_digest: String,
}

/// Coarse ADK scope gate. The runtime policy engine still evaluates the exact action.
#[derive(Debug, Clone, Default)]
pub struct ScopeAuthorizer {
    scopes: Vec<String>,
    principal_id: Option<String>,
    tenant_id: Option<String>,
}

/// Reason a [`ScopeAuthorizer`] rejected a [`ComputerUseAuthContext`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthorizationError {
    /// The caller lacks the scope required for the requested mode.
    #[error("missing required computer-use scope: {0}")]
    MissingScope(&'static str),
    /// The action's principal does not match the verified ADK identity.
    #[error("principal does not match the verified ADK identity")]
    PrincipalMismatch,
    /// The action's tenant does not match the verified ADK identity.
    #[error("tenant does not match the verified ADK identity")]
    TenantMismatch,
}

impl ScopeAuthorizer {
    /// Construct a scope gate from verified JWT/OIDC request scopes.
    pub fn new(scopes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            scopes: scopes.into_iter().map(Into::into).collect(),
            principal_id: None,
            tenant_id: None,
        }
    }

    /// Construct from identity already verified by adk-auth JWT/OIDC middleware.
    pub fn from_verified_identity(
        principal_id: impl Into<String>,
        tenant_id: Option<String>,
        scopes: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            scopes: scopes.into_iter().map(Into::into).collect(),
            principal_id: Some(principal_id.into()),
            tenant_id,
        }
    }

    /// Tenant identity already verified by adk-auth middleware. Graph/model
    /// state is never consulted for this value.
    pub fn verified_tenant_id(&self) -> Option<&str> {
        self.tenant_id.as_deref()
    }

    /// Verify coarse entitlement without treating it as runtime action approval.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorizationError::PrincipalMismatch`] or
    /// [`AuthorizationError::TenantMismatch`] when the context identity differs
    /// from the verified identity, or [`AuthorizationError::MissingScope`] when
    /// the required `computer:*` scope for the requested mode is absent.
    pub fn authorize(&self, context: &ComputerUseAuthContext) -> Result<(), AuthorizationError> {
        if self.principal_id.as_deref().is_some_and(|id| id != context.principal_id) {
            return Err(AuthorizationError::PrincipalMismatch);
        }
        if self.tenant_id.is_some() && self.tenant_id != context.tenant_id {
            return Err(AuthorizationError::TenantMismatch);
        }
        let required = match context.requested_mode {
            ExecutionMode::Shadow => "computer:plan",
            ExecutionMode::Background => "computer:execute:background",
            ExecutionMode::Foreground => "computer:execute:foreground",
        };
        check_scopes(&[required], &self.scopes)
            .map_err(|_| AuthorizationError::MissingScope(required))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_scope_does_not_follow_from_background_scope() {
        let authorizer = ScopeAuthorizer::new(["computer:execute:background"]);
        let context = ComputerUseAuthContext {
            principal_id: "p".into(),
            tenant_id: None,
            session_id: "s".into(),
            execution_group_id: "g".into(),
            requested_mode: ExecutionMode::Foreground,
            action_class: ActionClass::Navigate,
            target_app: None,
            target_window: None,
            policy_digest: "d".into(),
        };
        assert_eq!(
            authorizer.authorize(&context),
            Err(AuthorizationError::MissingScope("computer:execute:foreground"))
        );
    }

    #[test]
    fn verified_identity_must_match_runtime_principal_and_tenant() {
        let authorizer = ScopeAuthorizer::from_verified_identity(
            "verified",
            Some("tenant-a".into()),
            ["computer:execute:background"],
        );
        let mut context = ComputerUseAuthContext {
            principal_id: "attacker".into(),
            tenant_id: Some("tenant-a".into()),
            session_id: "s".into(),
            execution_group_id: "g".into(),
            requested_mode: ExecutionMode::Background,
            action_class: ActionClass::Navigate,
            target_app: None,
            target_window: None,
            policy_digest: "d".into(),
        };
        assert_eq!(authorizer.authorize(&context), Err(AuthorizationError::PrincipalMismatch));
        context.principal_id = "verified".into();
        context.tenant_id = Some("tenant-b".into());
        assert_eq!(authorizer.authorize(&context), Err(AuthorizationError::TenantMismatch));
    }
}
