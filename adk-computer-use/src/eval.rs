use crate::SessionEvent;
use adk_eval::{ToolTrajectoryScorer, ToolUse};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq)]
pub struct ComputerUseEvaluation {
    pub passed: bool,
    pub trajectory_score: f64,
    pub mutations: usize,
    pub committed: usize,
    pub violations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdkEvaluationSource {
    pub path: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdkEvaluationClaims {
    pub tests_passed: bool,
    pub auth_bound: bool,
    pub multimodal_evidence: bool,
    pub duplicate_mutations: u64,
    pub crash_points_covered: u64,
    pub test_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdkEvaluationReceipt {
    pub schema_version: u32,
    pub protocol: String,
    pub subject_version: String,
    pub generated_at: String,
    pub commands: Vec<String>,
    pub assertions: Vec<String>,
    pub claims: AdkEvaluationClaims,
    pub sources: Vec<AdkEvaluationSource>,
    pub source_digest: String,
    pub output_digest: String,
    pub receipt_digest: String,
}

fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Array(values) => {
            format!("[{}]", values.iter().map(canonical_json).collect::<Vec<_>>().join(","))
        }
        serde_json::Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        canonical_json(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        _ => serde_json::to_string(value).unwrap(),
    }
}

fn sha256(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

impl AdkEvaluationReceipt {
    pub fn seal(mut self) -> Result<Self, serde_json::Error> {
        self.receipt_digest.clear();
        self.receipt_digest = sha256(&canonical_json(&serde_json::to_value(&self)?));
        Ok(self)
    }

    pub fn verify(&self) -> bool {
        if self.schema_version != 1
            || self.protocol != "adk-rust-computer-use-v8-evaluation"
            || self.subject_version.is_empty()
            || self.commands.len() < 2
            || self.assertions.is_empty()
            || self.assertions.iter().collect::<HashSet<_>>().len() != self.assertions.len()
            || !self.claims.tests_passed
            || !self.claims.auth_bound
            || !self.claims.multimodal_evidence
            || self.claims.duplicate_mutations != 0
            || self.claims.crash_points_covered < 2
            || self.claims.test_count == 0
            || self.sources.is_empty()
            || !self.source_digest.starts_with("sha256:")
            || !self.output_digest.starts_with("sha256:")
        {
            return false;
        }
        self.clone().seal().is_ok_and(|sealed| sealed.receipt_digest == self.receipt_digest)
    }
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

#[cfg(test)]
mod receipt_tests {
    use super::*;

    fn receipt() -> AdkEvaluationReceipt {
        AdkEvaluationReceipt {
            schema_version: 1,
            protocol: "adk-rust-computer-use-v8-evaluation".into(),
            subject_version: "8.0.0".into(),
            generated_at: "2026-07-13T12:00:00Z".into(),
            commands: vec!["cargo test graph".into(), "cargo test multimodal".into()],
            assertions: vec!["graph.pre_effect_crash".into(), "graph.post_commit_crash".into()],
            claims: AdkEvaluationClaims {
                tests_passed: true,
                auth_bound: true,
                multimodal_evidence: true,
                duplicate_mutations: 0,
                crash_points_covered: 2,
                test_count: 2,
            },
            sources: vec![AdkEvaluationSource {
                path: "test.rs".into(),
                digest: format!("sha256:{}", "a".repeat(64)),
            }],
            source_digest: format!("sha256:{}", "b".repeat(64)),
            output_digest: format!("sha256:{}", "c".repeat(64)),
            receipt_digest: String::new(),
        }
        .seal()
        .unwrap()
    }

    #[test]
    fn evaluation_receipt_is_canonical_and_tamper_evident() {
        let value = receipt();
        assert!(value.verify());
        let round_trip: AdkEvaluationReceipt =
            serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
        assert!(round_trip.verify());
        let mut tampered = value;
        tampered.claims.duplicate_mutations = 1;
        assert!(!tampered.verify());
    }
}
