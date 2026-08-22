use std::collections::{BTreeSet, HashSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{RelationshipKind, TeamExecutionSnapshot, TeamExecutionStatus, TeamSpec};

/// Provider-free evaluation summary derived from a team execution receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeamExecutionAnalysis {
    /// Number of exact relationships declared by the specification.
    pub declared_relationships: usize,
    /// Number of distinct declared relationships observed in the receipt.
    pub covered_relationships: usize,
    /// Relationship coverage in basis points (10,000 = 100%).
    pub coverage_basis_points: u32,
    /// Delegation executions observed, including retries.
    pub delegations: usize,
    /// Handoff executions observed, including retries.
    pub handoffs: usize,
    /// Failed relationship executions.
    pub failed_edges: usize,
    /// Maximum causal relationship depth in the receipt.
    pub max_causal_depth: usize,
    /// Declared exact relationships that were not exercised.
    pub uncovered: Vec<String>,
}

/// Integrity failure found while validating an execution receipt for replay/evaluation.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum TeamReplayError {
    /// Receipt belongs to a different team.
    #[error("receipt belongs to team '{actual}', expected '{expected}'")]
    TeamMismatch {
        /// Expected team.
        expected: String,
        /// Receipt team.
        actual: String,
    },
    /// Relationship execution identifier is duplicated.
    #[error("duplicate relationship execution id '{0}'")]
    DuplicateEdgeId(String),
    /// Receipt contains an edge not declared by the spec.
    #[error("receipt contains undeclared {kind:?} edge from '{from}' to '{to}'")]
    UndeclaredEdge {
        /// Source member.
        from: String,
        /// Target member.
        to: String,
        /// Relationship semantics.
        kind: RelationshipKind,
    },
    /// A causal parent is absent or appears after its child.
    #[error("edge '{edge}' references unavailable causal parent '{parent}'")]
    InvalidParent {
        /// Child execution.
        edge: String,
        /// Invalid parent execution.
        parent: String,
    },
    /// A terminal timestamp precedes its start.
    #[error("edge '{0}' finishes before it starts")]
    InvalidTimestamp(String),
}

/// Validates receipt integrity against an immutable [`TeamSpec`].
pub fn validate_team_replay(
    spec: &TeamSpec,
    snapshot: &TeamExecutionSnapshot,
) -> std::result::Result<(), TeamReplayError> {
    if snapshot.team != spec.name {
        return Err(TeamReplayError::TeamMismatch {
            expected: spec.name.clone(),
            actual: snapshot.team.clone(),
        });
    }
    let declared: BTreeSet<_> = spec
        .relationships
        .iter()
        .map(|edge| (edge.from.as_str(), edge.to.as_str(), edge.kind))
        .collect();
    let mut seen = HashSet::new();
    for edge in &snapshot.edges {
        if !seen.insert(edge.id.as_str()) {
            return Err(TeamReplayError::DuplicateEdgeId(edge.id.clone()));
        }
        if !declared.contains(&(edge.from.as_str(), edge.to.as_str(), edge.kind)) {
            return Err(TeamReplayError::UndeclaredEdge {
                from: edge.from.clone(),
                to: edge.to.clone(),
                kind: edge.kind,
            });
        }
        if let Some(parent) = &edge.parent_id
            && !seen.contains(parent.as_str())
        {
            return Err(TeamReplayError::InvalidParent {
                edge: edge.id.clone(),
                parent: parent.clone(),
            });
        }
        if edge.finished_at_ms.is_some_and(|finished| finished < edge.started_at_ms) {
            return Err(TeamReplayError::InvalidTimestamp(edge.id.clone()));
        }
    }
    Ok(())
}

/// Computes deterministic topology coverage and failure metrics.
pub fn analyze_team_execution(
    spec: &TeamSpec,
    snapshot: &TeamExecutionSnapshot,
) -> TeamExecutionAnalysis {
    let declared: BTreeSet<_> = spec
        .relationships
        .iter()
        .map(|edge| (edge.from.clone(), edge.to.clone(), edge.kind))
        .collect();
    let covered: BTreeSet<_> =
        snapshot.edges.iter().map(|edge| (edge.from.clone(), edge.to.clone(), edge.kind)).collect();
    let covered_count = declared.intersection(&covered).count();
    let coverage_basis_points = if declared.is_empty() {
        10_000
    } else {
        u32::try_from(covered_count.saturating_mul(10_000) / declared.len()).unwrap_or(10_000)
    };
    let ids: std::collections::HashMap<_, _> =
        snapshot.edges.iter().map(|edge| (edge.id.as_str(), edge.parent_id.as_deref())).collect();
    let max_causal_depth = snapshot
        .edges
        .iter()
        .map(|edge| {
            let mut depth = 1;
            let mut parent = edge.parent_id.as_deref();
            let mut guard = HashSet::new();
            while let Some(id) = parent {
                if !guard.insert(id) {
                    break;
                }
                depth += 1;
                parent = ids.get(id).copied().flatten();
            }
            depth
        })
        .max()
        .unwrap_or(0);
    TeamExecutionAnalysis {
        declared_relationships: declared.len(),
        covered_relationships: covered_count,
        coverage_basis_points,
        delegations: snapshot
            .edges
            .iter()
            .filter(|edge| edge.kind == RelationshipKind::Delegate)
            .count(),
        handoffs: snapshot
            .edges
            .iter()
            .filter(|edge| edge.kind == RelationshipKind::Handoff)
            .count(),
        failed_edges: snapshot
            .edges
            .iter()
            .filter(|edge| edge.status == TeamExecutionStatus::Failed)
            .count(),
        max_causal_depth,
        uncovered: declared
            .difference(&covered)
            .map(|(from, to, kind)| format!("{from} -{kind:?}-> {to}"))
            .collect(),
    }
}
