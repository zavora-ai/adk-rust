//! Persona profile types for simulated user behavior.

use serde::{Deserialize, Serialize};

/// A structured definition of a simulated user's behavior, goals,
/// constraints, and communication style.
///
/// Used by [`super::UserSimulator`] to generate realistic user messages
/// during evaluation runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonaProfile {
    /// Unique name identifying this persona.
    pub name: String,
    /// Human-readable description of the persona.
    pub description: String,
    /// Behavioral traits controlling communication style.
    pub traits: PersonaTraits,
    /// Objectives the persona pursues during conversation.
    pub goals: Vec<String>,
    /// Topics or patterns the persona avoids.
    #[serde(default)]
    pub constraints: Vec<String>,
}

/// Behavioral traits that shape how a persona communicates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonaTraits {
    /// Free-form description of communication style (e.g., "direct and terse").
    pub communication_style: String,
    /// How verbose the persona's messages are.
    pub verbosity: Verbosity,
    /// The persona's domain expertise level.
    pub expertise_level: ExpertiseLevel,
}

/// Controls how verbose a persona's messages are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verbosity {
    /// Short, minimal responses.
    Terse,
    /// Standard conversational length.
    Normal,
    /// Detailed, elaborate responses.
    Verbose,
}

/// The persona's domain expertise level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExpertiseLevel {
    /// Beginner with limited domain knowledge.
    Novice,
    /// Moderate domain knowledge.
    Intermediate,
    /// Deep domain expertise.
    Expert,
}
