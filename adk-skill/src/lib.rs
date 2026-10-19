//! `adk-skill` discovers, parses, indexes, and selects agent skills defined by the
//! `agentskills.io` specification: Markdown files with YAML frontmatter under `.skills/` and
//! `.claude/skills/`, plus convention files such as `AGENTS.md` and `CLAUDE.md`.
//!
//! # Key Features
//! - **Discovery**: Recursively scans `.skills/` and `.claude/skills/` for skills, and the
//!   project tree for convention files.
//! - **Specification First**: Parses frontmatter for metadata, tool permissions, and references.
//! - **Selection**: Deterministic, weighted lexical scoring with tag filters and thresholds.
//! - **Dynamic Tooling**: Binds a skill's `allowed-tools` through a `ToolRegistry`.
//! - **Versioning & Hashing**: Content-based identifiers for reliable skill selection.
//!
#![doc = include_str!("../README.md")]

mod coordinator;
mod discovery;
mod error;
mod index;
mod injector;
mod model;
mod parser;
#[cfg(feature = "progressive-disclosure")]
mod progressive;
#[cfg(feature = "vertex-skill-registry")]
pub mod registry;
mod select;
mod writer;

pub use coordinator::{
    ContextCoordinator, CoordinatorConfig, ResolutionStrategy, SkillContext, ToolRegistry,
    ValidationMode,
};
pub use discovery::{
    discover_instruction_files, discover_instruction_files_with_extras, discover_skill_files,
    discover_skill_files_with_extras,
};
pub use error::{SkillError, SkillResult};
pub use index::{load_skill_index, load_skill_index_with_extras};
pub use injector::{
    SkillInjector, SkillInjectorConfig, apply_skill_injection, select_skill_prompt_block,
};
pub use model::{
    ParsedSkill, SelectionPolicy, SkillDocument, SkillFrontmatter, SkillIndex, SkillMatch,
    SkillSummary,
};
pub use parser::{parse_instruction_markdown, parse_skill_markdown};
#[cfg(feature = "progressive-disclosure")]
pub use progressive::{
    ActivatedSkill, DEFAULT_SKILL_SYSTEM_INSTRUCTION, ResourceAccessPolicy, SkillToolset,
    SkillToolsetConfig, build_skill_system_instruction,
};
pub use select::select_skills;
pub use writer::{SkillDraft, SkillWriter, validate_skill_name};
