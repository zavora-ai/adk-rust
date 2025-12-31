//! Access control with role-based permissions.

use crate::audit::{AuditEvent, AuditOutcome, AuditSink};
use crate::error::{AccessDenied, AuthError};
use crate::permission::Permission;
use crate::role::Role;
use std::collections::HashMap;
use std::sync::Arc;

/// Access control for checking permissions.
#[derive(Clone)]
pub struct AccessControl {
    /// Roles by name.
    roles: HashMap<String, Role>,
    /// User to role assignments.
    user_roles: HashMap<String, Vec<String>>,
    /// Optional audit sink.
    audit: Option<Arc<dyn AuditSink>>,
}

impl AccessControl {
    /// Create a new builder.
    pub fn builder() -> AccessControlBuilder {
        AccessControlBuilder::default()
    }

    /// Check if a user has access to a permission.
    pub fn check(&self, user: &str, permission: &Permission) -> Result<(), AccessDenied> {
        let role_names = self.user_roles.get(user);

        if let Some(role_names) = role_names {
            for role_name in role_names {
                if let Some(role) = self.roles.get(role_name) {
                    if role.can_access(permission) {
                        return Ok(());
                    }
                }
            }
        }

        Err(AccessDenied::new(user, permission.to_string()))
    }

    /// Check and log the access attempt.
    pub async fn check_and_audit(
        &self,
        user: &str,
        permission: &Permission,
    ) -> Result<(), AuthError> {
        let result = self.check(user, permission);

        // Log to audit sink if configured
        if let Some(audit) = &self.audit {
            let outcome = if result.is_ok() {
                AuditOutcome::Allowed
            } else {
                AuditOutcome::Denied
            };

            let event = match permission {
                Permission::Tool(name) => {
                    AuditEvent::tool_access(user, name.as_str(), outcome)
                }
                Permission::AllTools => {
                    AuditEvent::tool_access(user, "*", outcome)
                }
                Permission::Agent(name) => {
                    AuditEvent::agent_access(user, name.as_str(), outcome)
                }
                Permission::AllAgents => {
                    AuditEvent::agent_access(user, "*", outcome)
                }
            };

            audit.log(event).await?;
        }

        result.map_err(AuthError::from)
    }

    /// Get all roles assigned to a user.
    pub fn user_roles(&self, user: &str) -> Vec<&Role> {
        self.user_roles
            .get(user)
            .map(|names| {
                names
                    .iter()
                    .filter_map(|name| self.roles.get(name))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all role names.
    pub fn role_names(&self) -> Vec<&str> {
        self.roles.keys().map(|s| s.as_str()).collect()
    }

    /// Get a role by name.
    pub fn get_role(&self, name: &str) -> Option<&Role> {
        self.roles.get(name)
    }
}

/// Builder for AccessControl.
#[derive(Default)]
pub struct AccessControlBuilder {
    roles: HashMap<String, Role>,
    user_roles: HashMap<String, Vec<String>>,
    audit: Option<Arc<dyn AuditSink>>,
}

impl AccessControlBuilder {
    /// Add a role.
    pub fn role(mut self, role: Role) -> Self {
        self.roles.insert(role.name.clone(), role);
        self
    }

    /// Assign a role to a user.
    pub fn assign(mut self, user: impl Into<String>, role: impl Into<String>) -> Self {
        self.user_roles
            .entry(user.into())
            .or_default()
            .push(role.into());
        self
    }

    /// Set the audit sink.
    pub fn audit_sink(mut self, sink: impl AuditSink + 'static) -> Self {
        self.audit = Some(Arc::new(sink));
        self
    }

    /// Build the AccessControl.
    pub fn build(self) -> Result<AccessControl, AuthError> {
        // Validate all assigned roles exist
        for (user, roles) in &self.user_roles {
            for role in roles {
                if !self.roles.contains_key(role) {
                    return Err(AuthError::RoleNotFound(format!(
                        "Role '{}' assigned to user '{}' does not exist",
                        role, user
                    )));
                }
            }
        }

        Ok(AccessControl {
            roles: self.roles,
            user_roles: self.user_roles,
            audit: self.audit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_ac() -> AccessControl {
        let admin = Role::new("admin")
            .allow(Permission::AllTools)
            .allow(Permission::AllAgents);

        let user = Role::new("user")
            .allow(Permission::Tool("search".into()))
            .deny(Permission::Tool("exec".into()));

        AccessControl::builder()
            .role(admin)
            .role(user)
            .assign("alice", "admin")
            .assign("bob", "user")
            .build()
            .unwrap()
    }

    #[test]
    fn test_admin_has_full_access() {
        let ac = setup_ac();
        assert!(ac.check("alice", &Permission::Tool("anything".into())).is_ok());
        assert!(ac.check("alice", &Permission::AllTools).is_ok());
        assert!(ac.check("alice", &Permission::Agent("any".into())).is_ok());
    }

    #[test]
    fn test_user_limited_access() {
        let ac = setup_ac();
        // Can access search
        assert!(ac.check("bob", &Permission::Tool("search".into())).is_ok());
        // Cannot access exec (denied)
        assert!(ac.check("bob", &Permission::Tool("exec".into())).is_err());
        // Cannot access other tools
        assert!(ac.check("bob", &Permission::Tool("other".into())).is_err());
    }

    #[test]
    fn test_unknown_user_denied() {
        let ac = setup_ac();
        assert!(ac.check("unknown", &Permission::Tool("search".into())).is_err());
    }

    #[test]
    fn test_invalid_role_assignment() {
        let result = AccessControl::builder()
            .role(Role::new("admin"))
            .assign("alice", "nonexistent")
            .build();

        assert!(result.is_err());
    }

    #[test]
    fn test_multi_role_user() {
        let roles = vec![
            Role::new("reader").allow(Permission::Tool("read".into())),
            Role::new("writer").allow(Permission::Tool("write".into())),
        ];

        let ac = AccessControl::builder()
            .role(roles[0].clone())
            .role(roles[1].clone())
            .assign("bob", "reader")
            .assign("bob", "writer")
            .build()
            .unwrap();

        // Bob has both roles, can access both
        assert!(ac.check("bob", &Permission::Tool("read".into())).is_ok());
        assert!(ac.check("bob", &Permission::Tool("write".into())).is_ok());
    }
}
