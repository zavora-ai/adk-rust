use crate::{ActionClass, ExecutionMode};
use adk_auth::check_scopes;
use thiserror::Error;

/// Authenticated identity and exact operation context forwarded to v8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerUseAuthContext {
    pub principal_id: String,
    pub tenant_id: Option<String>,
    pub session_id: String,
    pub execution_group_id: String,
    pub requested_mode: ExecutionMode,
    pub action_class: ActionClass,
    pub target_app: Option<String>,
    pub target_window: Option<String>,
    pub policy_digest: String,
}

/// Coarse ADK scope gate. The v8 policy engine still evaluates the exact action.
#[derive(Debug, Clone, Default)]
pub struct ScopeAuthorizer {
    scopes: Vec<String>,
    principal_id: Option<String>,
    tenant_id: Option<String>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthorizationError {
    #[error("missing required computer-use scope: {0}")]
    MissingScope(&'static str),
    #[error("v8 principal does not match the verified ADK identity")]
    PrincipalMismatch,
    #[error("v8 tenant does not match the verified ADK identity")]
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

    /// Verify coarse entitlement without treating it as v8 action approval.
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
    fn verified_identity_must_match_v8_principal_and_tenant() {
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
