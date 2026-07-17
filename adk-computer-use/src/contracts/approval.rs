//! Disclosure-safe approval grants returned by the runtime approval boundary.

use super::action::{ActionClass, ExecutionMode};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Scope of an [`ApprovalGrant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalGrantScope {
    /// Authority bound to exactly one action digest and a single use.
    ExactAction,
    /// Authority bound to a bounded reversible semantic edit operation.
    SessionOperation,
}

/// Read-only, disclosure-safe authority returned by the runtime approval boundary.
///
/// Values are validated on deserialization: digests must be lowercase SHA-256
/// hex, use accounting is bounded, and each scope enforces its own binding
/// rules (`ExactAction` binds one digest/use; `SessionOperation` must be a
/// bounded reversible `set_value`/`fill_form` edit). The bearer token never
/// enters ADK or model state — only this read-only descriptor does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawApprovalGrant", into = "RawApprovalGrant")]
pub struct ApprovalGrant {
    grant_id: String,
    scope: ApprovalGrantScope,
    principal_id: String,
    session_id: String,
    action_digest: String,
    scope_digest: String,
    policy_digest: String,
    tool: String,
    operation: String,
    action_class: ActionClass,
    mode: ExecutionMode,
    issued_at: String,
    expires_at: String,
    remaining_uses: u32,
    consumed_by_action_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawApprovalGrant {
    grant_id: String,
    scope: ApprovalGrantScope,
    principal_id: String,
    session_id: String,
    action_digest: String,
    scope_digest: String,
    policy_digest: String,
    tool: String,
    operation: String,
    action_class: ActionClass,
    mode: ExecutionMode,
    issued_at: String,
    expires_at: String,
    remaining_uses: u32,
    consumed_by_action_ids: Vec<String>,
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl TryFrom<RawApprovalGrant> for ApprovalGrant {
    type Error = String;

    fn try_from(raw: RawApprovalGrant) -> Result<Self, Self::Error> {
        if raw.grant_id.is_empty()
            || raw.principal_id.is_empty()
            || raw.session_id.is_empty()
            || raw.policy_digest.is_empty()
            || raw.issued_at.is_empty()
            || raw.expires_at.is_empty()
        {
            return Err("approval grant identity, policy, and timestamps must not be empty".into());
        }
        if !is_sha256_hex(&raw.action_digest) || !is_sha256_hex(&raw.scope_digest) {
            return Err("approval grant digests must be lowercase SHA-256 hex".into());
        }
        if raw.remaining_uses > 100 || raw.consumed_by_action_ids.len() > 100 {
            return Err("approval grant use accounting exceeds the wire limit".into());
        }
        let unique: HashSet<&str> = raw.consumed_by_action_ids.iter().map(String::as_str).collect();
        if unique.len() != raw.consumed_by_action_ids.len()
            || raw.consumed_by_action_ids.iter().any(String::is_empty)
        {
            return Err("approval grant consumed action IDs must be nonempty and unique".into());
        }
        match raw.scope {
            ApprovalGrantScope::ExactAction => {
                if raw.scope_digest != raw.action_digest
                    || raw.remaining_uses > 1
                    || raw.consumed_by_action_ids.len() > 1
                {
                    return Err("exact-action approval must bind one action digest and use".into());
                }
            }
            ApprovalGrantScope::SessionOperation => {
                if !matches!(raw.tool.as_str(), "set_value" | "fill_form")
                    || raw.operation.is_empty()
                    || raw.action_class != ActionClass::EditReversible
                    || raw.remaining_uses + raw.consumed_by_action_ids.len() as u32 > 20
                {
                    return Err(
                        "session-operation approval must be a bounded reversible semantic edit"
                            .into(),
                    );
                }
            }
        }
        Ok(Self {
            grant_id: raw.grant_id,
            scope: raw.scope,
            principal_id: raw.principal_id,
            session_id: raw.session_id,
            action_digest: raw.action_digest,
            scope_digest: raw.scope_digest,
            policy_digest: raw.policy_digest,
            tool: raw.tool,
            operation: raw.operation,
            action_class: raw.action_class,
            mode: raw.mode,
            issued_at: raw.issued_at,
            expires_at: raw.expires_at,
            remaining_uses: raw.remaining_uses,
            consumed_by_action_ids: raw.consumed_by_action_ids,
        })
    }
}

impl From<ApprovalGrant> for RawApprovalGrant {
    fn from(value: ApprovalGrant) -> Self {
        Self {
            grant_id: value.grant_id,
            scope: value.scope,
            principal_id: value.principal_id,
            session_id: value.session_id,
            action_digest: value.action_digest,
            scope_digest: value.scope_digest,
            policy_digest: value.policy_digest,
            tool: value.tool,
            operation: value.operation,
            action_class: value.action_class,
            mode: value.mode,
            issued_at: value.issued_at,
            expires_at: value.expires_at,
            remaining_uses: value.remaining_uses,
            consumed_by_action_ids: value.consumed_by_action_ids,
        }
    }
}

impl ApprovalGrant {
    /// The unique grant identifier.
    pub fn grant_id(&self) -> &str {
        &self.grant_id
    }

    /// The scope of authority this grant confers.
    pub fn scope(&self) -> ApprovalGrantScope {
        self.scope
    }

    /// The digest the grant's scope is bound to.
    pub fn scope_digest(&self) -> &str {
        &self.scope_digest
    }

    /// Remaining uses before the grant is exhausted.
    pub fn remaining_uses(&self) -> u32 {
        self.remaining_uses
    }
}
