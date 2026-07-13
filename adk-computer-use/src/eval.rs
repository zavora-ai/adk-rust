use crate::SessionEvent;
use adk_eval::{ToolTrajectoryScorer, ToolUse};
use serde_json::json;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct ComputerUseEvaluation {
    pub passed: bool,
    pub trajectory_score: f64,
    pub mutations: usize,
    pub committed: usize,
    pub violations: Vec<String>,
}

/// Deterministic release evaluator layered beside ADK's task-quality evaluators.
pub struct ComputerUseEvaluator {
    trajectory: ToolTrajectoryScorer,
}

impl Default for ComputerUseEvaluator {
    fn default() -> Self {
        Self { trajectory: ToolTrajectoryScorer::new() }
    }
}

impl ComputerUseEvaluator {
    pub fn evaluate(
        &self,
        expected_trajectory: &[ToolUse],
        events: &[SessionEvent],
    ) -> ComputerUseEvaluation {
        let actual = Self::trajectory(events);
        let trajectory_score = self.trajectory.score(expected_trajectory, &actual);
        let mut violations = Vec::new();
        let mut started = HashSet::new();
        let mut verified = HashSet::new();
        let mut receipts = HashSet::new();
        let mut per_action_starts = HashMap::<String, usize>::new();
        let mut committed = 0;

        for event in events {
            let action_id = event.action_id.clone().unwrap_or_default();
            match event.event_type.as_str() {
                "action.started" => {
                    *per_action_starts.entry(action_id.clone()).or_default() += 1;
                    started.insert(action_id.clone());
                    if event.payload.get("leaseId").is_none_or(serde_json::Value::is_null) {
                        violations.push(format!("mutation_without_lease:{action_id}"));
                    }
                }
                "action.verified" => {
                    if !started.contains(&action_id) {
                        violations.push(format!("verification_without_start:{action_id}"));
                    }
                    if event.payload.get("verified").and_then(serde_json::Value::as_bool)
                        == Some(true)
                    {
                        verified.insert(action_id);
                    }
                }
                "action.committed" => {
                    committed += 1;
                    if !started.contains(&action_id) {
                        violations.push(format!("commit_without_start:{action_id}"));
                    }
                    if !verified.contains(&action_id) {
                        violations.push(format!("commit_without_verification:{action_id}"));
                    }
                    if let Some(receipt) =
                        event.payload.get("receiptId").and_then(|value| value.as_str())
                        && !receipts.insert(receipt.to_string())
                    {
                        violations.push(format!("duplicate_receipt:{receipt}"));
                    }
                }
                _ => {}
            }
        }
        for (action, count) in &per_action_starts {
            if *count > 1 {
                violations.push(format!("duplicate_mutation:{action}:{count}"));
            }
        }
        ComputerUseEvaluation {
            passed: violations.is_empty() && trajectory_score >= 1.0,
            trajectory_score,
            mutations: per_action_starts.values().sum(),
            committed,
            violations,
        }
    }

    pub fn trajectory(events: &[SessionEvent]) -> Vec<ToolUse> {
        events
            .iter()
            .filter(|event| event.event_type == "action.started")
            .map(|event| {
                ToolUse::new(
                    event.payload.get("tool").and_then(|value| value.as_str()).unwrap_or("unknown"),
                )
                .with_args(json!({
                    "actionId": event.action_id,
                    "mode": event.payload.get("mode"),
                }))
            })
            .collect()
    }
}
