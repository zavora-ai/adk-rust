//! Composition pipeline types for the composable scaffolding engine.
//!
//! The composition pipeline resolves a template + addons + provider into a
//! `CompositionManifest` containing all information needed to generate a project.

use serde::Serialize;
use std::collections::BTreeSet;
use std::fmt;

use crate::provider::get_provider_config;
use crate::registry::TemplateRegistry;

/// A resolved dependency to include in the generated `Cargo.toml`.
#[derive(Debug, Clone)]
pub struct ResolvedDependency {
    /// Crate name.
    pub crate_name: String,
    /// Version requirement.
    pub version: String,
    /// Features to enable.
    pub features: Vec<String>,
    /// Whether to use default features.
    pub default_features: bool,
}

/// A file to be written to disk during project generation.
#[derive(Debug, Clone)]
pub struct GeneratedFile {
    /// Relative path within the project directory.
    pub path: String,
    /// File content.
    pub content: String,
}

/// The resolved output of combining a template with addons and a provider.
#[derive(Debug, Clone, Serialize)]
pub struct CompositionManifest {
    /// The resolved template name.
    pub template_name: String,
    /// Names of all applied addons.
    pub addons: Vec<String>,
    /// The selected provider name.
    pub provider: String,
    /// Optional model ID override (replaces provider default).
    pub model_override: Option<String>,
    /// Union of all required Cargo features.
    pub feature_set: BTreeSet<String>,
    /// All resolved crate dependencies.
    #[serde(skip)]
    pub dependencies: Vec<ResolvedDependency>,
    /// All files to generate.
    #[serde(skip)]
    pub files: Vec<GeneratedFile>,
    /// Environment variables to document.
    pub env_vars: Vec<(String, String)>,
    /// Warnings generated during composition (e.g., deprecation notices).
    pub warnings: Vec<String>,
}

/// Errors that can occur during composition resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositionError {
    /// The requested template name was not found in the registry.
    UnknownTemplate(String),
    /// The requested addon name was not found in the registry.
    UnknownAddon(String),
    /// An addon is incompatible with the selected template.
    IncompatibleAddon { addon: String, template: String, reason: String },
    /// Two selected addons conflict with each other.
    ConflictingAddons { addon_a: String, addon_b: String },
}

impl fmt::Display for CompositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompositionError::UnknownTemplate(name) => {
                write!(f, "unknown template '{name}'. Run 'cargo adk templates' to see options")
            }
            CompositionError::UnknownAddon(name) => {
                write!(f, "unknown addon '{name}'. Run 'cargo adk addons' to see options")
            }
            CompositionError::IncompatibleAddon { addon, template, reason } => {
                write!(f, "addon '{addon}' is incompatible with template '{template}': {reason}")
            }
            CompositionError::ConflictingAddons { addon_a, addon_b } => {
                write!(f, "addons '{addon_a}' and '{addon_b}' cannot be used together")
            }
        }
    }
}

impl std::error::Error for CompositionError {}

/// Output for `--dry-run` mode showing what would be generated.
#[derive(Debug, Clone, Serialize)]
pub struct DryRunOutput {
    /// Files that would be created.
    pub files: Vec<DryRunFile>,
    /// Cargo features that would be enabled.
    pub feature_set: Vec<String>,
    /// Crate dependencies that would be added.
    pub dependencies: Vec<String>,
    /// Environment variables that would be documented.
    pub env_vars: Vec<String>,
}

/// A single file entry in dry-run output.
#[derive(Debug, Clone, Serialize)]
pub struct DryRunFile {
    /// Relative path of the file.
    pub path: String,
    /// Size of the file content in bytes.
    pub size_bytes: usize,
}

/// Resolves a template + addons + provider into a [`CompositionManifest`].
///
/// This is the core composition pipeline function. It validates all inputs,
/// checks compatibility, computes the feature set, resolves dependencies,
/// and returns a manifest ready for code generation.
///
/// # Errors
///
/// Returns [`CompositionError`] if:
/// - The template name is not found in the registry (including aliases)
/// - Any addon name is not found in the registry
/// - An addon is incompatible with the selected template
/// - Two selected addons conflict with each other
///
/// # Example
///
/// ```rust,ignore
/// let registry = TemplateRegistry::builtin();
/// let manifest = resolve_composition(&registry, "llm", &["telemetry", "sessions"], "gemini")?;
/// assert!(manifest.feature_set.contains("telemetry"));
/// assert!(manifest.feature_set.contains("sessions"));
/// ```
pub fn resolve_composition(
    registry: &TemplateRegistry,
    template: &str,
    addons: &[&str],
    provider: &str,
) -> Result<CompositionManifest, CompositionError> {
    // 1. Resolve template (check aliases too)
    let resolved_template = registry
        .resolve_template(template)
        .ok_or_else(|| CompositionError::UnknownTemplate(template.to_string()))?;

    // 2. Resolve provider configuration
    let provider_config = get_provider_config(provider).map_err(|_| {
        CompositionError::UnknownTemplate(format!(
            "unknown provider '{provider}'. Check supported providers"
        ))
    })?;

    // 3. Resolve and validate each addon
    let mut resolved_addons = Vec::with_capacity(addons.len());
    for &addon_name in addons {
        let addon = registry
            .capability_addons
            .iter()
            .find(|a| a.name == addon_name)
            .ok_or_else(|| CompositionError::UnknownAddon(addon_name.to_string()))?;

        // Check if template lists this addon as incompatible
        if resolved_template.incompatible_addons.contains(&addon_name) {
            return Err(CompositionError::IncompatibleAddon {
                addon: addon_name.to_string(),
                template: resolved_template.name.to_string(),
                reason: format!(
                    "template '{}' declares '{}' as incompatible",
                    resolved_template.name, addon_name
                ),
            });
        }

        // Check if addon lists this template as incompatible
        if addon.incompatible_with.contains(&resolved_template.name) {
            return Err(CompositionError::IncompatibleAddon {
                addon: addon_name.to_string(),
                template: resolved_template.name.to_string(),
                reason: format!(
                    "addon '{}' declares template '{}' as incompatible",
                    addon_name, resolved_template.name
                ),
            });
        }

        resolved_addons.push(addon);
    }

    // 4. Check addon-addon conflicts
    for i in 0..resolved_addons.len() {
        for j in (i + 1)..resolved_addons.len() {
            let addon_a = resolved_addons[i];
            let addon_b = resolved_addons[j];

            if addon_a.incompatible_with.contains(&addon_b.name)
                || addon_b.incompatible_with.contains(&addon_a.name)
            {
                return Err(CompositionError::ConflictingAddons {
                    addon_a: addon_a.name.to_string(),
                    addon_b: addon_b.name.to_string(),
                });
            }
        }
    }

    // 5. Compute feature set as union of template + addons + provider
    let mut feature_set = BTreeSet::new();
    for &feature in &resolved_template.required_features {
        feature_set.insert(feature.to_string());
    }
    for addon in &resolved_addons {
        for &feature in &addon.required_features {
            feature_set.insert(feature.to_string());
        }
    }
    feature_set.insert(provider_config.feature_flag.to_string());

    // 6. Collect dependencies from the template and all addons
    let mut dependencies = Vec::new();
    for dep in &resolved_template.additional_deps {
        dependencies.push(ResolvedDependency {
            crate_name: dep.crate_name.to_string(),
            version: dep.version.to_string(),
            features: dep.features.iter().map(|f| f.to_string()).collect(),
            default_features: true,
        });
    }
    for addon in &resolved_addons {
        for dep in &addon.additional_deps {
            dependencies.push(ResolvedDependency {
                crate_name: dep.crate_name.to_string(),
                version: dep.version.to_string(),
                features: dep.features.iter().map(|f| f.to_string()).collect(),
                default_features: true,
            });
        }
    }

    // 7. Sort addons by init_priority for code generation ordering
    let mut sorted_addons = resolved_addons.clone();
    sorted_addons.sort_by_key(|a| a.init_priority);

    // 8. Collect env_vars from provider (if requires_api_key) + all addons
    let mut env_vars = Vec::new();
    if provider_config.requires_api_key && !provider_config.env_var.is_empty() {
        env_vars.push((
            provider_config.env_var.to_string(),
            format!("Your {} API key", provider_config.name),
        ));
    }
    for addon in &sorted_addons {
        for &(key, description) in &addon.code_fragments.env_vars {
            env_vars.push((key.to_string(), description.to_string()));
        }
    }

    // 9. Return CompositionManifest (files will be empty — filled by codegen)
    Ok(CompositionManifest {
        template_name: resolved_template.name.to_string(),
        addons: sorted_addons.iter().map(|a| a.name.to_string()).collect(),
        provider: provider_config.name.to_string(),
        model_override: None,
        feature_set,
        dependencies,
        files: Vec::new(),
        env_vars,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> TemplateRegistry {
        TemplateRegistry::builtin()
    }

    #[test]
    fn test_unknown_template_returns_error() {
        let reg = registry();
        let result = resolve_composition(&reg, "nonexistent", &[], "gemini");
        assert!(matches!(
            result,
            Err(CompositionError::UnknownTemplate(ref name)) if name == "nonexistent"
        ));
    }

    #[test]
    fn test_unknown_addon_returns_error() {
        let reg = registry();
        let result = resolve_composition(&reg, "llm", &["nonexistent_addon"], "gemini");
        assert!(matches!(
            result,
            Err(CompositionError::UnknownAddon(ref name)) if name == "nonexistent_addon"
        ));
    }

    #[test]
    fn test_successful_resolution_with_no_addons() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &[], "gemini").unwrap();

        assert_eq!(manifest.template_name, "llm");
        assert_eq!(manifest.provider, "gemini");
        assert!(manifest.addons.is_empty());
        // Template "llm" requires "minimal", provider "gemini" adds "gemini"
        assert!(manifest.feature_set.contains("minimal"));
        assert!(manifest.feature_set.contains("gemini"));
    }

    #[test]
    fn test_successful_resolution_with_addons() {
        let reg = registry();
        let manifest =
            resolve_composition(&reg, "llm", &["telemetry", "sessions"], "openai").unwrap();

        assert_eq!(manifest.template_name, "llm");
        assert_eq!(manifest.provider, "openai");
        // Addons sorted by priority: telemetry(10) before sessions(30)
        assert_eq!(manifest.addons, vec!["telemetry", "sessions"]);
        // Feature set is union
        assert!(manifest.feature_set.contains("minimal"));
        assert!(manifest.feature_set.contains("openai"));
        assert!(manifest.feature_set.contains("telemetry"));
        assert!(manifest.feature_set.contains("sessions"));
    }

    #[test]
    fn test_alias_resolution() {
        let reg = registry();
        // "basic" is an alias for "llm"
        let manifest = resolve_composition(&reg, "basic", &[], "gemini").unwrap();
        assert_eq!(manifest.template_name, "llm");
    }

    #[test]
    fn test_addon_ordering_by_priority() {
        let reg = registry();
        // Pass addons in reverse priority order
        let manifest =
            resolve_composition(&reg, "llm", &["server", "telemetry", "auth"], "gemini").unwrap();

        // Should be sorted by priority: telemetry(10), auth(20), server(90)
        assert_eq!(manifest.addons, vec!["telemetry", "auth", "server"]);
    }

    #[test]
    fn test_env_vars_collected_from_provider_and_addons() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &[], "openai").unwrap();

        // OpenAI requires API key, so env_vars should contain OPENAI_API_KEY
        assert!(manifest.env_vars.iter().any(|(key, _)| key == "OPENAI_API_KEY"));
    }

    #[test]
    fn test_ollama_no_api_key_env_var() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &[], "ollama").unwrap();

        // Ollama doesn't require an API key
        assert!(manifest.env_vars.is_empty());
    }

    #[test]
    fn test_graph_template_features() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "graph", &[], "gemini").unwrap();

        // Graph template requires "minimal" and "graph"
        assert!(manifest.feature_set.contains("minimal"));
        assert!(manifest.feature_set.contains("graph"));
        assert!(manifest.feature_set.contains("gemini"));
    }

    #[test]
    fn test_feature_set_is_union() {
        let reg = registry();
        let manifest =
            resolve_composition(&reg, "graph", &["mcp", "telemetry"], "anthropic").unwrap();

        // Union of: graph(minimal, graph) + mcp(tools, mcp) + telemetry(telemetry) + anthropic
        assert!(manifest.feature_set.contains("minimal"));
        assert!(manifest.feature_set.contains("graph"));
        assert!(manifest.feature_set.contains("tools"));
        assert!(manifest.feature_set.contains("mcp"));
        assert!(manifest.feature_set.contains("telemetry"));
        assert!(manifest.feature_set.contains("anthropic"));
    }
}
