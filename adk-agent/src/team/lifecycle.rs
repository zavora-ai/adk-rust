use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{RelationshipKind, TeamRuntimeError};

/// Lifecycle boundary exposed by portable team execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TeamLifecyclePhase {
    /// The compiled team root is starting or finishing.
    Team,
    /// One concrete member is starting or finishing.
    Member,
    /// One exact delegate or handoff relationship is starting or finishing.
    Relationship,
}

/// Immutable context supplied to a [`TeamLifecycleHook`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeamLifecycleContext {
    /// Team root name.
    pub team: String,
    /// Stable root invocation identifier.
    pub invocation_id: String,
    /// Lifecycle boundary.
    pub phase: TeamLifecyclePhase,
    /// Member associated with a member lifecycle boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// Relationship execution identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_id: Option<String>,
    /// Relationship source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Relationship target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Relationship semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<RelationshipKind>,
    /// One-based attempt number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
}

/// Result observed after a team lifecycle boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "status")]
pub enum TeamLifecycleOutcome {
    /// The operation completed normally.
    Succeeded,
    /// The operation failed.
    Failed {
        /// Stable ADK error code when one is available.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// Human-readable failure message.
        message: String,
    },
    /// Policy terminated the operation before execution.
    Terminated {
        /// Policy reason.
        reason: String,
    },
}

/// Decision returned before a team lifecycle boundary executes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "decision")]
pub enum TeamLifecycleDecision {
    /// Continue normal execution.
    Continue,
    /// Deny the exact pending operation.
    Terminate {
        /// Auditable policy reason.
        reason: String,
    },
}

/// Async team execution hook.
///
/// Runner's ordinary plugins still wrap the complete agent run. This hook adds
/// the team-specific relationship boundary that generic plugins cannot infer.
#[async_trait]
pub trait TeamLifecycleHook: Send + Sync {
    /// Stable hook name used in diagnostics and telemetry.
    fn name(&self) -> &str;

    /// Lower values run first before an operation and last after it.
    fn priority(&self) -> i32 {
        0
    }

    /// Runs immediately before the described operation.
    async fn before(
        &self,
        _context: &TeamLifecycleContext,
    ) -> adk_core::Result<TeamLifecycleDecision> {
        Ok(TeamLifecycleDecision::Continue)
    }

    /// Runs after success, failure, or policy termination.
    async fn after(
        &self,
        _context: &TeamLifecycleContext,
        _outcome: &TeamLifecycleOutcome,
    ) -> adk_core::Result<()> {
        Ok(())
    }
}

pub(crate) struct TeamLifecycleManager {
    hooks: Vec<Arc<dyn TeamLifecycleHook>>,
}

impl TeamLifecycleManager {
    pub(crate) fn new(mut hooks: Vec<Arc<dyn TeamLifecycleHook>>) -> Self {
        hooks.sort_by_key(|hook| hook.priority());
        Self { hooks }
    }

    pub(crate) async fn before(
        &self,
        context: &TeamLifecycleContext,
    ) -> adk_core::Result<TeamLifecycleDecision> {
        for hook in &self.hooks {
            match hook.before(context).await.map_err(|error| TeamRuntimeError::LifecycleHook {
                hook: hook.name().to_string(),
                phase: context.phase,
                message: error.to_string(),
            })? {
                TeamLifecycleDecision::Continue => {}
                decision @ TeamLifecycleDecision::Terminate { .. } => return Ok(decision),
            }
        }
        Ok(TeamLifecycleDecision::Continue)
    }

    pub(crate) async fn after(
        &self,
        context: &TeamLifecycleContext,
        outcome: &TeamLifecycleOutcome,
    ) -> adk_core::Result<()> {
        for hook in self.hooks.iter().rev() {
            hook.after(context, outcome).await.map_err(|error| {
                TeamRuntimeError::LifecycleHook {
                    hook: hook.name().to_string(),
                    phase: context.phase,
                    message: error.to_string(),
                }
            })?;
        }
        Ok(())
    }
}
