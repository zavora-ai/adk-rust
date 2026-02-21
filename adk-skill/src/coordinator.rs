//! # Context Coordinator
//!
//! This module implements the **Context Engineering Pipeline** for `adk-skill`.
//! It bridges the gap between skill *selection* (scoring) and skill *execution* (tool binding),
//! guaranteeing that an agent never receives instructions to use a tool that isn't bound.
//!
//! ## The Problem
//! Injecting a skill's body into a prompt is insufficient. If the skill says "use `transfer_call`"
//! but the tool isn't registered, the LLM will hallucinate the action — the "Phantom Tool" failure.
//!
//! ## The Solution
//! The `ContextCoordinator` runs a three-stage pipeline:
//! 1. **Selection** — `select_skills` scores all skills against the query.
//! 2. **Validation** — Checks that the skill's `allowed-tools` exist in the host's `ToolRegistry`.
//! 3. **Context Engineering** — Constructs a `SkillContext` with both the system instruction and
//!    the resolved `Vec<Arc<dyn Tool>>`, delivered as a single atomic unit.
//!
//! Host applications provide a [`ToolRegistry`] implementation to map tool names to concrete
//! instances. See [`DESIGN.md`](../DESIGN.md) for the full architectural rationale.

use crate::error::SkillResult;
use crate::model::{SelectionPolicy, SkillIndex, SkillMatch, SkillSummary};
use crate::select::select_skills;
pub use adk_core::{ResolvedContext, Tool, ToolRegistry, ValidationMode};
use std::sync::Arc;

/// The output of the context engineering pipeline.
///
/// Encapsulates both the LLM's cognitive frame (instructions) and its physical
/// capabilities (tools) as a single, atomic unit. This guarantees that the agent
/// never receives a prompt telling it to use a tool that isn't bound.
#[derive(Clone)]
pub struct SkillContext {
    /// The inner validated context (instruction + tools).
    pub inner: ResolvedContext,
    /// The score and metadata that triggered this context, for observability.
    pub provenance: SkillMatch,
}

impl std::ops::Deref for SkillContext {
    type Target = ResolvedContext;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl std::fmt::Debug for SkillContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillContext")
            .field("inner", &self.inner)
            .field("provenance", &self.provenance)
            .finish()
    }
}

// ToolRegistry is now in adk_core

/// Configuration for the `ContextCoordinator`.
#[derive(Debug, Clone)]
pub struct CoordinatorConfig {
    /// The selection policy used for scoring skills.
    pub policy: SelectionPolicy,
    /// Maximum characters to include from the skill body in the system instruction.
    pub max_instruction_chars: usize,
    /// How to handle skills that request unavailable tools.
    pub validation_mode: ValidationMode,
}

/// Defines a strategy for resolving a skill into a context.
#[derive(Debug, Clone)]
pub enum ResolutionStrategy {
    /// Resolve by exact skill name. Fails if name not found.
    ByName(String),
    /// Resolve by scoring against a user query. Fails if no skill meets threshold.
    ByQuery(String),
    /// Resolve by looking for a skill with a specific tag (e.g., "fallback", "default").
    /// Picks the best-scored match among skills with this tag.
    ByTag(String),
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            policy: SelectionPolicy::default(),
            max_instruction_chars: 8000,
            validation_mode: ValidationMode::default(),
        }
    }
}

/// The Context Engineering Engine for `adk-skill`.
///
/// Orchestrates the full pipeline: scoring → tool validation → context construction.
/// Guarantees that the emitted `SkillContext` always has its `active_tools` aligned
/// with what the `system_instruction` tells the LLM it can do.
///
/// # Pipeline
/// 1. **Selection**: Scores skills against the query using `select_skills`.
/// 2. **Validation**: Checks that requested `allowed-tools` exist in the `ToolRegistry`.
/// 3. **Context Engineering**: Constructs a structured `system_instruction` from the body.
/// 4. **Emission**: Returns a safe `SkillContext` or `None` if no valid match.
pub struct ContextCoordinator {
    index: Arc<SkillIndex>,
    registry: Arc<dyn ToolRegistry>,
    config: CoordinatorConfig,
}

impl ContextCoordinator {
    /// Create a new coordinator from a skill index, tool registry, and config.
    pub fn new(
        index: Arc<SkillIndex>,
        registry: Arc<dyn ToolRegistry>,
        config: CoordinatorConfig,
    ) -> Self {
        Self { index, registry, config }
    }

    /// Build a `SkillContext` for the given query.
    ///
    /// Runs the full pipeline: score → validate tools → engineer context.
    /// Returns `None` if no skill meets the policy threshold or tool validation fails.
    pub fn build_context(&self, query: &str) -> Option<SkillContext> {
        // 1. Score all skills and get candidates (top_k)
        let candidates = select_skills(&self.index, query, &self.config.policy);

        // 2. Try each candidate in rank order
        for candidate in candidates {
            match self.try_resolve(&candidate) {
                Ok(context) => return Some(context),
                Err(_) => continue, // In strict mode, try next candidate
            }
        }

        None
    }

    /// Build a `SkillContext` for a skill looked up by exact name.
    ///
    /// This bypasses scoring entirely and is useful when the caller already
    /// knows which skill to load (e.g., from a config field).
    pub fn build_context_by_name(&self, name: &str) -> Option<SkillContext> {
        let skill = self.index.find_by_name(name)?;
        let summary = SkillSummary::from(skill);
        let skill_match = SkillMatch { score: f32::MAX, skill: summary };

        self.try_resolve(&skill_match).ok()
    }

    /// Resolve a `SkillContext` using a prioritized list of strategies.
    ///
    /// This is the "Standard-Compliant" entry point for context resolution.
    /// It attempts each strategy in order and returns the first successful `SkillContext`.
    ///
    /// # Example
    /// ```rust,ignore
    /// coordinator.resolve(&[
    ///     ResolutionStrategy::ByName("emergency".into()),
    ///     ResolutionStrategy::ByQuery("I smell gas".into()),
    ///     ResolutionStrategy::ByTag("fallback".into()),
    /// ]);
    /// ```
    pub fn resolve(&self, strategies: &[ResolutionStrategy]) -> Option<SkillContext> {
        for strategy in strategies {
            let result = match strategy {
                ResolutionStrategy::ByName(name) => self.build_context_by_name(name),
                ResolutionStrategy::ByQuery(query) => self.build_context(query),
                ResolutionStrategy::ByTag(tag) => {
                    // Filter index by tag, then score against an empty/generic query
                    let candidates = select_skills(
                        &self.index,
                        "", // Generic query for tag matching
                        &SelectionPolicy {
                            include_tags: vec![tag.clone()],
                            top_k: 1,
                            min_score: 0.0, // Tags are binary, score doesn't matter much here
                            ..self.config.policy.clone()
                        },
                    );
                    candidates.first().and_then(|m| self.try_resolve(m).ok())
                }
            };

            if let Some(ctx) = result {
                return Some(ctx);
            }
        }
        None
    }

    /// Attempt to resolve tools and build a context for a single candidate.
    fn try_resolve(&self, candidate: &SkillMatch) -> SkillResult<SkillContext> {
        let allowed = &candidate.skill.allowed_tools;

        // 2a. Resolve tools from registry
        let mut active_tools: Vec<Arc<dyn Tool>> = Vec::new();
        let mut missing: Vec<String> = Vec::new();

        for tool_name in allowed {
            if let Some(tool) = self.registry.resolve(tool_name) {
                active_tools.push(tool);
            } else {
                missing.push(tool_name.clone());
            }
        }

        // 2b. Validate based on mode
        if !missing.is_empty() {
            match self.config.validation_mode {
                ValidationMode::Strict => {
                    return Err(crate::error::SkillError::Validation(format!(
                        "Skill '{}' requires tools not in registry: {:?}",
                        candidate.skill.name, missing
                    )));
                }
                ValidationMode::Permissive => {
                    // Continue with partial tools — missing tools are silently omitted.
                    // In production, consumers should monitor `provenance.skill.allowed_tools`
                    // against `active_tools` to detect gaps.
                }
            }
        }

        // 3. Engineer the system instruction
        let matched_skill = self.index.find_by_id(&candidate.skill.id).ok_or_else(|| {
            crate::error::SkillError::IndexError(format!(
                "Matched skill not found in index: {}",
                candidate.skill.name
            ))
        })?;

        let system_instruction =
            matched_skill.engineer_instruction(self.config.max_instruction_chars, &active_tools);

        Ok(SkillContext {
            inner: ResolvedContext { system_instruction, active_tools },
            provenance: candidate.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::load_skill_index;
    use async_trait::async_trait;
    use serde_json::Value;
    use std::fs;

    // -- Test Tool --
    struct MockTool {
        tool_name: String,
    }

    #[async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &str {
            "mock tool"
        }
        async fn execute(
            &self,
            _ctx: Arc<dyn adk_core::ToolContext>,
            _args: Value,
        ) -> adk_core::Result<Value> {
            Ok(Value::Null)
        }
    }

    // -- Test Registry --
    struct TestRegistry {
        available: Vec<String>,
    }

    impl ToolRegistry for TestRegistry {
        fn resolve(&self, tool_name: &str) -> Option<Arc<dyn Tool>> {
            if self.available.contains(&tool_name.to_string()) {
                Some(Arc::new(MockTool { tool_name: tool_name.to_string() }))
            } else {
                None
            }
        }
    }

    fn setup_index(tools: &[&str]) -> (tempfile::TempDir, SkillIndex) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".skills")).unwrap();

        let tools_yaml = if tools.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = tools.iter().map(|t| format!("  - {}", t)).collect();
            format!("allowed-tools:\n{}\n", items.join("\n"))
        };

        fs::write(
            root.join(".skills/emergency.md"),
            format!(
                "---\nname: emergency\ndescription: Handle gas and water emergencies\ntags:\n  - plumber\n{}\n---\nYou are an emergency dispatcher. Route calls for gas leaks and floods.",
                tools_yaml
            ),
        )
        .unwrap();

        let index = load_skill_index(root).unwrap();
        (temp, index)
    }

    #[test]
    fn build_context_scores_and_resolves_tools() {
        let (_tmp, index) = setup_index(&["knowledge", "transfer_call"]);
        let registry = TestRegistry { available: vec!["knowledge".into(), "transfer_call".into()] };

        let coordinator = ContextCoordinator::new(
            Arc::new(index),
            Arc::new(registry),
            CoordinatorConfig {
                policy: SelectionPolicy { top_k: 1, min_score: 0.1, ..Default::default() },
                ..Default::default()
            },
        );

        let ctx = coordinator.build_context("gas emergency").unwrap();
        assert_eq!(ctx.active_tools.len(), 2);
        assert!(ctx.system_instruction.contains("[skill:emergency]"));
        assert!(ctx.system_instruction.contains("knowledge, transfer_call"));
        assert!(ctx.system_instruction.contains("emergency dispatcher"));
    }

    #[test]
    fn strict_mode_rejects_missing_tools() {
        let (_tmp, index) = setup_index(&["knowledge", "nonexistent_tool"]);
        let registry = TestRegistry { available: vec!["knowledge".into()] };

        let coordinator = ContextCoordinator::new(
            Arc::new(index),
            Arc::new(registry),
            CoordinatorConfig {
                policy: SelectionPolicy { top_k: 1, min_score: 0.1, ..Default::default() },
                validation_mode: ValidationMode::Strict,
                ..Default::default()
            },
        );

        let ctx = coordinator.build_context("gas emergency");
        assert!(ctx.is_none(), "Strict mode should reject skills with missing tools");
    }

    #[test]
    fn permissive_mode_binds_available_tools() {
        let (_tmp, index) = setup_index(&["knowledge", "nonexistent_tool"]);
        let registry = TestRegistry { available: vec!["knowledge".into()] };

        let coordinator = ContextCoordinator::new(
            Arc::new(index),
            Arc::new(registry),
            CoordinatorConfig {
                policy: SelectionPolicy { top_k: 1, min_score: 0.1, ..Default::default() },
                validation_mode: ValidationMode::Permissive,
                ..Default::default()
            },
        );

        let ctx = coordinator.build_context("gas emergency").unwrap();
        assert_eq!(ctx.active_tools.len(), 1);
        assert_eq!(ctx.active_tools[0].name(), "knowledge");
    }

    #[test]
    fn build_context_by_name_bypasses_scoring() {
        let (_tmp, index) = setup_index(&["knowledge"]);
        let registry = TestRegistry { available: vec!["knowledge".into()] };

        let coordinator = ContextCoordinator::new(
            Arc::new(index),
            Arc::new(registry),
            CoordinatorConfig::default(),
        );

        let ctx = coordinator.build_context_by_name("emergency").unwrap();
        assert_eq!(ctx.active_tools.len(), 1);
        assert!(ctx.system_instruction.contains("[skill:emergency]"));
    }

    #[test]
    fn no_tools_skill_returns_empty_active_tools() {
        let (_tmp, index) = setup_index(&[]);
        let registry = TestRegistry { available: vec![] };

        let coordinator = ContextCoordinator::new(
            Arc::new(index),
            Arc::new(registry),
            CoordinatorConfig {
                policy: SelectionPolicy { top_k: 1, min_score: 0.1, ..Default::default() },
                ..Default::default()
            },
        );

        let ctx = coordinator.build_context("gas emergency").unwrap();
        assert!(ctx.active_tools.is_empty());
        assert!(ctx.system_instruction.contains("emergency dispatcher"));
    }

    #[test]
    fn resolve_cascades_through_strategies() {
        let (_tmp, index) = setup_index(&["knowledge"]);
        let registry = TestRegistry { available: vec!["knowledge".into()] };

        let coordinator = ContextCoordinator::new(
            Arc::new(index),
            Arc::new(registry),
            CoordinatorConfig::default(),
        );

        // 1. Name match should work
        let ctx = coordinator.resolve(&[ResolutionStrategy::ByName("emergency".into())]);
        assert!(ctx.is_some());

        // 2. Query match should work
        let ctx = coordinator.resolve(&[ResolutionStrategy::ByQuery("gas emergency".into())]);
        assert!(ctx.is_some());

        // 3. Tag match should work
        let ctx = coordinator.resolve(&[ResolutionStrategy::ByTag("plumber".into())]);
        assert!(ctx.is_some(), "Should resolve by 'plumber' tag");

        // 4. Multiple strategies, first success wins
        let ctx = coordinator.resolve(&[
            ResolutionStrategy::ByName("nonexistent".into()),
            ResolutionStrategy::ByTag("plumber".into()),
        ]);
        assert_eq!(ctx.unwrap().provenance.skill.name, "emergency");
    }
}
