//! Skills through progressive disclosure (spec 3.1 §7.8, feature `skills`).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::{
    LlmRequest, ReadonlyContext, Result, Tool, ToolContext, ToolEffect, ToolRegistry, Toolset,
    async_trait,
};
use adk_skill::{SkillIndex, SkillToolset, SkillToolsetConfig};
use serde_json::{Value, json};

/// How skills reach the model (spec 3.1 §7.8).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_harness::SkillsMode::default(), adk_harness::SkillsMode::Progressive);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SkillsMode {
    /// `list_skills`, `load_skill` and `load_skill_resource` tools; the default.
    #[default]
    Progressive,
    /// Lexical injection of the best-matching skill into the user turn; opt-in.
    Inject,
    /// No skills.
    Off,
}

/// Where skills come from and how they load (spec 3.1 §7.8).
///
/// # Example
///
/// ```rust
/// use adk_harness::{SkillsConfig, SkillsMode};
///
/// let config = SkillsConfig::default();
/// assert_eq!(config.mode, SkillsMode::Progressive);
/// assert_eq!(config.extra_roots[0], std::path::PathBuf::from(".adk/skills"));
/// ```
#[derive(Clone)]
pub struct SkillsConfig {
    /// How skills reach the model.
    pub mode: SkillsMode,
    /// Extra skill directories; relative entries resolve against the workspace root.
    /// Default: `<ws>/.adk/skills`, `~/.adk/skills`. `.skills/` and `.claude/skills/` are
    /// always searched.
    pub extra_roots: Vec<PathBuf>,
    /// Tools a skill's `allowed-tools` may activate; activated tools are governed as
    /// `External` (spec 3.1 §9.2 I34).
    pub pool: Vec<Arc<dyn Tool>>,
    /// The progressive toolset's limits.
    pub toolset: SkillToolsetConfig,
    /// The Vertex AI Skill Registry, adding `search_skills`.
    #[cfg(feature = "skills-registry")]
    pub registry: Option<Arc<adk_skill::registry::SkillRegistryClient>>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        let mut extra_roots = vec![PathBuf::from(".adk/skills")];
        if let Some(home) = dirs::home_dir() {
            extra_roots.push(home.join(".adk").join("skills"));
        }
        Self {
            mode: SkillsMode::Progressive,
            extra_roots,
            pool: Vec::new(),
            toolset: SkillToolsetConfig::default(),
            #[cfg(feature = "skills-registry")]
            registry: None,
        }
    }
}

impl fmt::Debug for SkillsConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SkillsConfig")
            .field("mode", &self.mode)
            .field("extra_roots", &self.extra_roots)
            .field(
                "pool",
                &self.pool.iter().map(|tool| tool.name().to_string()).collect::<Vec<_>>(),
            )
            .field("toolset", &self.toolset)
            .finish()
    }
}

/// The tools a skill may activate.
struct PoolRegistry {
    pool: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry for PoolRegistry {
    fn resolve(&self, tool_name: &str) -> Option<Arc<dyn Tool>> {
        self.pool.iter().find(|tool| tool.name() == tool_name).cloned()
    }

    fn available_tools(&self) -> Vec<String> {
        self.pool.iter().map(|tool| tool.name().to_string()).collect()
    }
}

/// The skill index of a workspace without convention documents (`AGENTS.md` and the
/// like), which the harness loads as instructions instead (spec 3.1 §7.7, §7.8).
pub(crate) fn load_index(root: &Path, config: &SkillsConfig) -> Result<SkillIndex> {
    let extra: Vec<PathBuf> = config
        .extra_roots
        .iter()
        .map(|dir| if dir.is_absolute() { dir.clone() } else { root.join(dir) })
        .collect();
    let index = adk_skill::load_skill_index_with_extras(root, &extra)
        .map_err(|error| adk_core::AdkError::config(format!("load skills: {error}")))?;
    let skills = index
        .skills()
        .iter()
        .filter(|skill| !skill.tags.iter().any(|tag| tag == "convention"))
        .cloned()
        .collect();
    Ok(SkillIndex::new(skills))
}

/// What the builder attaches for a skills configuration.
pub(crate) enum SkillsWiring {
    None,
    Toolset(Arc<AuditedSkillToolset>),
    Inject(SkillIndex),
}

/// The progressive skill toolset, with `load_skill` recording `harness.skill.activated`
/// (spec 3.1 §9.3).
pub(crate) struct AuditedSkillToolset {
    inner: Arc<SkillToolset>,
    index: Arc<SkillIndex>,
    root: PathBuf,
}

#[async_trait]
impl Toolset for AuditedSkillToolset {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn tools(&self, ctx: Arc<dyn ReadonlyContext>) -> Result<Vec<Arc<dyn Tool>>> {
        let tools = self.inner.tools(ctx).await?;
        Ok(tools
            .into_iter()
            .map(|tool| {
                if tool.name() == "load_skill" {
                    Arc::new(AuditedLoadSkill {
                        inner: tool,
                        index: self.index.clone(),
                        root: self.root.clone(),
                    }) as Arc<dyn Tool>
                } else {
                    tool
                }
            })
            .collect())
    }

    async fn process_llm_request(
        &self,
        ctx: Arc<dyn ReadonlyContext>,
        request: &mut LlmRequest,
    ) -> Result<()> {
        self.inner.process_llm_request(ctx, request).await
    }

    async fn close(&self) {
        self.inner.close().await;
    }
}

/// `load_skill`, declared exactly as the skill toolset declares it, recording the
/// activation after the skill loads.
struct AuditedLoadSkill {
    inner: Arc<dyn Tool>,
    index: Arc<SkillIndex>,
    root: PathBuf,
}

#[async_trait]
impl Tool for AuditedLoadSkill {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn description(&self) -> &str {
        self.inner.description()
    }
    fn declaration(&self) -> Value {
        self.inner.declaration()
    }
    fn enhanced_description(&self) -> String {
        self.inner.enhanced_description()
    }
    fn is_long_running(&self) -> bool {
        self.inner.is_long_running()
    }
    fn is_builtin(&self) -> bool {
        self.inner.is_builtin()
    }
    fn parameters_schema(&self) -> Option<Value> {
        self.inner.parameters_schema()
    }
    fn response_schema(&self) -> Option<Value> {
        self.inner.response_schema()
    }
    fn annotations(&self) -> Option<&Value> {
        self.inner.annotations()
    }
    fn origin(&self) -> &str {
        self.inner.origin()
    }
    fn required_scopes(&self) -> &[&str] {
        self.inner.required_scopes()
    }
    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }
    fn effect(&self) -> ToolEffect {
        self.inner.effect()
    }

    fn egress(&self, args: &serde_json::Value) -> adk_core::egress::ToolEgress {
        self.inner.egress(args)
    }
    fn timeout_override(&self) -> Option<Option<std::time::Duration>> {
        self.inner.timeout_override()
    }
    fn is_concurrency_safe(&self) -> bool {
        self.inner.is_concurrency_safe()
    }
    fn is_agent_delegation(&self) -> bool {
        self.inner.is_agent_delegation()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let loaded = self.inner.execute(ctx.clone(), args).await?;
        if let Some(audit) = crate::audit::HarnessAudit::for_tool(ctx.as_ref()) {
            let id = loaded.get("skillId").and_then(Value::as_str).unwrap_or_default();
            let document = self.index.find_by_id(id);
            let source = document.map(|document| {
                document
                    .path
                    .strip_prefix(&self.root)
                    .unwrap_or(&document.path)
                    .display()
                    .to_string()
            });
            audit
                .record(
                    "harness.skill.activated",
                    json!({
                        "skillId": id,
                        "hash": loaded.get("hash"),
                        "sourcePath": source,
                        "allowedTools": document.map(|document| document.allowed_tools.clone()),
                        "functionCallId": ctx.function_call_id(),
                    }),
                )
                .await?;
        }
        Ok(loaded)
    }
}

/// Builds the skills wiring of `config` over the workspace at `root` (spec 3.1 §7.8).
pub(crate) fn wire(root: &Path, config: &SkillsConfig) -> Result<SkillsWiring> {
    if config.mode == SkillsMode::Off {
        return Ok(SkillsWiring::None);
    }
    let index = load_index(root, config)?;
    match config.mode {
        SkillsMode::Off => Ok(SkillsWiring::None),
        SkillsMode::Inject if index.is_empty() => Ok(SkillsWiring::None),
        SkillsMode::Inject => Ok(SkillsWiring::Inject(index)),
        SkillsMode::Progressive => {
            #[cfg(feature = "skills-registry")]
            let has_registry = config.registry.is_some();
            #[cfg(not(feature = "skills-registry"))]
            let has_registry = false;
            if index.is_empty() && !has_registry {
                return Ok(SkillsWiring::None);
            }
            let registry: Arc<dyn ToolRegistry> =
                Arc::new(PoolRegistry { pool: config.pool.clone() });
            let index = Arc::new(index);
            #[allow(unused_mut)]
            let mut toolset = SkillToolset::new(index.clone(), registry, config.toolset.clone());
            #[cfg(feature = "skills-registry")]
            if let Some(client) = &config.registry {
                toolset = toolset.with_registry_client(client.clone());
            }
            Ok(SkillsWiring::Toolset(Arc::new(AuditedSkillToolset {
                inner: Arc::new(toolset),
                index,
                root: root.to_path_buf(),
            })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{ToolContext, ToolEffect, ToolOrigin};

    /// `load_skill` as the skill toolset declares it: its own declaration and annotations.
    #[derive(Debug)]
    struct Declared;

    #[async_trait]
    impl Tool for Declared {
        fn name(&self) -> &str {
            "load_skill"
        }

        fn description(&self) -> &str {
            "loads a skill"
        }

        fn declaration(&self) -> Value {
            json!({
                "name": "load_skill",
                "description": "loads a skill",
                "parameters": { "type": "object", "properties": { "name": { "type": "string" } } },
                "x-skills": 3
            })
        }

        fn annotations(&self) -> Option<&Value> {
            static HINTS: std::sync::LazyLock<Value> =
                std::sync::LazyLock::new(|| json!({ "readOnlyHint": true }));
            Some(&HINTS)
        }

        fn origin(&self) -> &str {
            "skills:project"
        }

        fn effect(&self) -> ToolEffect {
            ToolEffect::ReadOnly
        }

        async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn the_audited_load_skill_keeps_the_wrapped_tools_origin_and_declaration_digest() {
        let inner: Arc<dyn Tool> = Arc::new(Declared);
        let audited = AuditedLoadSkill {
            inner: inner.clone(),
            index: Arc::new(SkillIndex::new(Vec::new())),
            root: PathBuf::new(),
        };
        assert_eq!(ToolOrigin::of(&audited), ToolOrigin::of(inner.as_ref()));
        assert_eq!(audited.origin(), "skills:project");
        assert_eq!(audited.annotations(), inner.annotations());
        assert_eq!(audited.effect(), ToolEffect::ReadOnly);
    }
}
