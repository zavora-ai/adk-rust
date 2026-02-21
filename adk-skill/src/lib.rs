//! `adk-skill` is the engine for specification-driven agent skills in the Zenith ecosystem.
//!
//! It implements the `agentskills.io` standard, allowing developers to define agent personalities,
//! capabilities, and tool permissions in structured Markdown files.
//!
//! # Key Features
//! - **Discovery**: Recursively scans the workspace for `.skill.md` and `skill.md` files.
//! - **Specification First**: Parses frontmatter for metadata, tool permissions, and external references.
//! - **Dynamic Tooling**: Provides the metadata needed for agents to configure their own toolsets at runtime.
//! - **Versioning & Hashing**: Content-based unique identifiers for reliable persona selection.
//!
//! # Example
//! Skills are defined in Markdown with YAML frontmatter:
//! ```markdown
//! ---
//! name: search-expert
//! description: Expert in semantic and keyword search.
//! allowed-tools:
//!   - knowledge
//!   - web_search
//! ---
//! Use tools to find information...
//! ```

#![doc = include_str!("../README.md")]

mod coordinator;
mod discovery;
mod error;
mod index;
mod injector;
mod model;
mod parser;
mod select;

pub use coordinator::{
    ContextCoordinator, CoordinatorConfig, ResolutionStrategy, SkillContext, ToolRegistry,
    ValidationMode,
};
pub use discovery::{discover_instruction_files, discover_skill_files};
pub use error::{SkillError, SkillResult};
pub use index::load_skill_index;
pub use injector::{
    SkillInjector, SkillInjectorConfig, apply_skill_injection, select_skill_prompt_block,
};
pub use model::{
    ParsedSkill, SelectionPolicy, SkillDocument, SkillFrontmatter, SkillIndex, SkillMatch,
    SkillSummary,
};
pub use parser::{parse_instruction_markdown, parse_skill_markdown};
pub use select::select_skills;
