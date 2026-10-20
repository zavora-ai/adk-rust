//! Skills through progressive disclosure (spec 3.1 §7.8, feature `skills`).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_core::{Result, Tool, ToolRegistry};
use adk_skill::{SkillIndex, SkillToolset, SkillToolsetConfig};

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
    Toolset(Arc<SkillToolset>),
    Inject(SkillIndex),
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
            #[allow(unused_mut)]
            let mut toolset = SkillToolset::new(Arc::new(index), registry, config.toolset.clone());
            #[cfg(feature = "skills-registry")]
            if let Some(client) = &config.registry {
                toolset = toolset.with_registry_client(client.clone());
            }
            Ok(SkillsWiring::Toolset(Arc::new(toolset)))
        }
    }
}
