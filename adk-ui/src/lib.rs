pub mod a2ui;
pub mod catalog_registry;
pub mod kit;
pub mod prompts;
pub mod schema;
pub mod templates;
pub mod tools;
pub mod toolset;
pub mod validation;

pub use a2ui::*;
pub use catalog_registry::{CatalogArtifact, CatalogError, CatalogRegistry, CatalogSource};
pub use kit::{KitArtifacts, KitGenerator, KitSpec};
pub use prompts::{UI_AGENT_PROMPT, UI_AGENT_PROMPT_SHORT};
pub use schema::*;
pub use templates::{StatItem, TemplateData, UiTemplate, UserData, render_template};
pub use tools::*;
pub use toolset::UiToolset;
pub use validation::{Validate, ValidationError, validate_ui_response};
