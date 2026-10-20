//! Model posture: reasoning effort per mode and role, and context caching
//! (spec 3.1 §7.23).
//!
//! How much effort a model spends and whether its prompt prefix is cached decide the
//! harness's cost and latency more than anything in the tool loop. [`EffortPolicy`] names
//! an [`Effort`] for every mode and for the member, distiller and sampling models;
//! `HarnessAgentBuilder::effort` sets it and `HarnessAgent::effort` reads it.
//!
//! Until spec 3.1 §10 C4 lands (an `effort` field on `GenerateContentConfig`, deferred as
//! open question 6), providers read effort when their client is built, so the policy
//! applies where clients are built — per session, not per model call. The surfaces map a
//! level through the `From<adk_core::Effort>` conversions in `adk-model` and `adk-gemini`:
//!
//! | `Effort` | Anthropic | OpenAI | Gemini | Ollama, mistral.rs |
//! |---|---|---|---|---|
//! | `Low` | `Effort::Low`, `ThinkingMode::Adaptive` | `ReasoningEffort::Low` | `ThinkingLevel::Low` | unchanged |
//! | `Medium` | `Medium` | `Medium` | `Medium` | unchanged |
//! | `High` | `High` | `High` | `High` | unchanged |
//!
//! Caching is on by default where a provider supports it: Anthropic and Bedrock cache the
//! prefix on their own, OpenAI needs nothing, and a Gemini model given to
//! `HarnessAgentBuilder::cache_capable` gets the runner's explicit context cache with
//! `ContextCacheConfig::default()`; `without_context_caching` turns that off.

pub use adk_core::Effort;
use serde::{Deserialize, Serialize};

use crate::mode::HarnessMode;

/// The reasoning effort of each mode and model role (spec 3.1 §7.23).
///
/// The policy never raises a level above `High` on its own; `xhigh`, `max`, `minimal` and
/// `none` are reachable only through the provider configuration the operator passes
/// (`customize_llm` or the client's own builder).
///
/// # Example
///
/// ```rust
/// use adk_harness::{Effort, EffortPolicy, HarnessMode};
///
/// let policy = EffortPolicy::default();
/// assert_eq!(policy.for_mode(HarnessMode::Plan), Effort::High);
/// assert_eq!(policy.for_mode(HarnessMode::Ask), Effort::Medium);
/// assert_eq!(policy.members, Effort::Low);
///
/// let flat = EffortPolicy::uniform(Effort::Low);
/// assert!(HarnessMode::ALL.iter().all(|mode| flat.for_mode(*mode) == Effort::Low));
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct EffortPolicy {
    /// ReadOnly sessions.
    pub read_only: Effort,
    /// Plan sessions: the plan is worth thinking about.
    pub plan: Effort,
    /// Ask sessions.
    pub ask: Effort,
    /// Auto sessions.
    pub auto: Effort,
    /// Team members (spec 3.1 §7.10).
    pub members: Effort,
    /// The long-term memory distiller (spec 3.1 §7.15.2).
    pub distiller: Effort,
    /// MCP sampling requests (spec 3.1 §7.21).
    pub sampling: Effort,
}

impl Default for EffortPolicy {
    fn default() -> Self {
        Self {
            read_only: Effort::Medium,
            plan: Effort::High,
            ask: Effort::Medium,
            auto: Effort::Medium,
            members: Effort::Low,
            distiller: Effort::Low,
            sampling: Effort::Low,
        }
    }
}

impl EffortPolicy {
    /// The same level everywhere: what `--effort` sets for a session.
    pub const fn uniform(effort: Effort) -> Self {
        Self {
            read_only: effort,
            plan: effort,
            ask: effort,
            auto: effort,
            members: effort,
            distiller: effort,
            sampling: effort,
        }
    }

    /// The harness model's effort in `mode`.
    pub const fn for_mode(&self, mode: HarnessMode) -> Effort {
        match mode {
            HarnessMode::ReadOnly => self.read_only,
            HarnessMode::Plan => self.plan,
            HarnessMode::Ask => self.ask,
            HarnessMode::Auto => self.auto,
        }
    }

    /// One line for banners and `/effort`: the mode levels, then the roles.
    pub fn describe(&self) -> String {
        format!(
            "read-only {}, plan {}, ask {}, auto {}; members {}, distiller {}, sampling {}",
            self.read_only,
            self.plan,
            self.ask,
            self.auto,
            self.members,
            self.distiller,
            self.sampling
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_spec_table() {
        let policy = EffortPolicy::default();
        assert_eq!(
            (
                policy.read_only,
                policy.plan,
                policy.ask,
                policy.auto,
                policy.members,
                policy.distiller,
                policy.sampling
            ),
            (
                Effort::Medium,
                Effort::High,
                Effort::Medium,
                Effort::Medium,
                Effort::Low,
                Effort::Low,
                Effort::Low
            )
        );
    }

    #[test]
    fn a_partial_toml_table_fills_the_rest_from_the_defaults() {
        let policy: EffortPolicy =
            serde_json::from_value(serde_json::json!({ "plan": "low", "members": "high" }))
                .unwrap();
        assert_eq!(
            policy,
            EffortPolicy { plan: Effort::Low, members: Effort::High, ..Default::default() }
        );
        assert!(
            serde_json::from_value::<EffortPolicy>(serde_json::json!({ "ask": "xhigh" })).is_err()
        );
    }

    #[test]
    fn describe_names_every_field() {
        let text = EffortPolicy::default().describe();
        for part in [
            "read-only medium",
            "plan high",
            "ask medium",
            "auto medium",
            "members low",
            "distiller low",
            "sampling low",
        ] {
            assert!(text.contains(part), "{text}");
        }
    }
}
