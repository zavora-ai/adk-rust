//! Versioned cross-runtime deterministic safety corpus shared with the
//! TypeScript fake-desktop harness.

use serde::{Deserialize, Serialize};

/// Versioned cross-runtime deterministic safety corpus.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyCorpus {
    /// Schema version of the corpus.
    pub schema_version: u32,
    /// Human-readable description of the corpus.
    pub description: String,
    /// The scenarios in the corpus.
    pub scenarios: Vec<SafetyScenario>,
}

/// A single deterministic safety scenario and its expected outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyScenario {
    /// Unique scenario identifier.
    pub id: String,
    /// The injected fault under test.
    pub fault: String,
    /// The expected outcome after the fault.
    pub expected: SafetyExpectation,
}

/// Expected effects, restores, and status for a [`SafetyScenario`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SafetyExpectation {
    /// Expected session status, when asserted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Expected error, when asserted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Expected receipt status, when asserted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_status: Option<String>,
    /// Expected number of physical effects.
    pub effects: u32,
    /// Expected number of restores/rollbacks.
    pub restores: u32,
    /// Expected number of effects on replay, when asserted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_effects: Option<u32>,
}
