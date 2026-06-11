//! Code generation engine for the composable scaffolding system.
//!
//! This module generates all project files from a [`CompositionManifest`]:
//! `main.rs`, `Cargo.toml`, `.env.example`, `README.md`, and `.gitignore`.
//!
//! The generated code uses `tracing` for logging, `anyhow` for error handling,
//! and follows ADK-Rust best practices.

use crate::composition::{CompositionManifest, GeneratedFile};
use crate::provider::get_provider_config;
use crate::registry::TemplateRegistry;

/// Current ADK-Rust version, read from this crate's own version at compile time.
pub const ADK_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Generate all project files from a composition manifest.
///
/// Returns a vector of [`GeneratedFile`] entries ready to be written to disk.
///
/// # Arguments
///
/// * `manifest` - The resolved composition manifest from the pipeline
/// * `project_name` - The project/crate name
///
/// # Example
///
/// ```rust,ignore
/// let files = generate_project(&manifest, "my-agent");
/// for file in &files {
///     println!("{}: {} bytes", file.path, file.content.len());
/// }
/// ```
pub fn generate_project(manifest: &CompositionManifest, project_name: &str) -> Vec<GeneratedFile> {
    generate_project_with_registry(&TemplateRegistry::builtin(), manifest, project_name)
}

/// Like [`generate_project`], but uses the given registry so custom templates
/// (loaded via `--template-dir`) contribute their code fragments.
pub fn generate_project_with_registry(
    registry: &TemplateRegistry,
    manifest: &CompositionManifest,
    project_name: &str,
) -> Vec<GeneratedFile> {
    let mut files = vec![
        GeneratedFile {
            path: "Cargo.toml".to_string(),
            content: generate_cargo_toml(manifest, project_name),
        },
        GeneratedFile {
            path: "src/main.rs".to_string(),
            content: generate_main_rs_with_registry(registry, manifest, project_name),
        },
        GeneratedFile { path: ".env.example".to_string(), content: generate_env_example(manifest) },
        GeneratedFile {
            path: "README.md".to_string(),
            content: generate_readme(manifest, project_name),
        },
        GeneratedFile { path: ".gitignore".to_string(), content: generate_gitignore() },
    ];

    // Append additional files contributed by the template and addons.
    if let Some(template) = registry.resolve_template(&manifest.template_name) {
        for fragment in &template.code_fragments.additional_files {
            files.push(GeneratedFile {
                path: fragment.path.to_string(),
                content: fragment.content.to_string(),
            });
        }
    }
    for addon_name in &manifest.addons {
        if let Some(addon) = registry.capability_addons.iter().find(|a| a.name == *addon_name) {
            for fragment in &addon.code_fragments.additional_files {
                files.push(GeneratedFile {
                    path: fragment.path.to_string(),
                    content: fragment.content.to_string(),
                });
            }
        }
    }

    files
}

/// Generate `Cargo.toml` with minimal dependencies from the composition manifest.
///
/// Uses edition 2024 and the current ADK_VERSION. Features are the union of
/// all template + addon + provider features.
pub fn generate_cargo_toml(manifest: &CompositionManifest, project_name: &str) -> String {
    let features: Vec<&str> = manifest.feature_set.iter().map(|s| s.as_str()).collect();
    let features_str = features.iter().map(|f| format!("\"{f}\"")).collect::<Vec<_>>().join(", ");

    let mut output = format!(
        r#"[package]
name = "{project_name}"
version = "0.1.0"
edition = "2024"

[dependencies]
adk-rust = {{ version = "{ADK_VERSION}", default-features = false, features = [{features_str}] }}
tokio = {{ version = "1", features = ["full"] }}
dotenvy = "0.15"
tracing = "0.1"
tracing-subscriber = {{ version = "0.3", features = ["env-filter"] }}
anyhow = "1"
"#
    );

    // Add additional dependencies from addons
    for dep in &manifest.dependencies {
        if dep.features.is_empty() {
            output.push_str(&format!("{} = \"{}\"\n", dep.crate_name, dep.version));
        } else {
            let dep_features =
                dep.features.iter().map(|f| format!("\"{f}\"")).collect::<Vec<_>>().join(", ");
            output.push_str(&format!(
                "{} = {{ version = \"{}\", features = [{dep_features}] }}\n",
                dep.crate_name, dep.version
            ));
        }
    }

    output
}

/// Fallback agent construction used when a template has no (or placeholder)
/// `agent_construction` fragment. Uses fully qualified `Arc` because the
/// fallback path cannot rely on template-provided imports.
fn placeholder_agent_construction(project_name: &str) -> String {
    format!(
        r#"    let agent: std::sync::Arc<dyn Agent> = std::sync::Arc::new(
        LlmAgentBuilder::new("{project_name}")
            .description("An AI assistant")
            .instruction("You are a helpful assistant.")
            .model(std::sync::Arc::new(model))
            .build()?,
    );"#,
    )
}

/// Generate `src/main.rs` with proper composition of template and addons.
///
/// The generated code merges:
/// - Provider model initialization
/// - Template agent construction
/// - Addon imports (sorted by priority)
/// - Addon initialization (sorted by priority)
/// - Addon builder calls
///
/// The server addon uses `std::env::var("PORT")` for port binding.
pub fn generate_main_rs(manifest: &CompositionManifest, project_name: &str) -> String {
    generate_main_rs_with_registry(&TemplateRegistry::builtin(), manifest, project_name)
}

/// Like [`generate_main_rs`], but uses the given registry so custom templates
/// (loaded via `--template-dir`) contribute their code fragments.
pub fn generate_main_rs_with_registry(
    registry: &TemplateRegistry,
    manifest: &CompositionManifest,
    project_name: &str,
) -> String {
    // Resolve provider config for model init code
    let provider_config = get_provider_config(&manifest.provider).ok();

    // Collect addon code fragments sorted by priority
    let mut sorted_addons: Vec<_> = manifest
        .addons
        .iter()
        .filter_map(|addon_name| registry.capability_addons.iter().find(|a| a.name == *addon_name))
        .collect();
    sorted_addons.sort_by_key(|a| a.init_priority);

    // Resolve template for agent construction
    let template = registry.resolve_template(&manifest.template_name);

    // Build imports section
    let mut imports = Vec::new();
    imports.push("use adk_rust::prelude::*;".to_string());

    // Add template imports
    if let Some(tmpl) = template {
        for imp in &tmpl.code_fragments.imports {
            if !imp.is_empty() {
                imports.push(imp.to_string());
            }
        }
    }

    // Add addon imports (sorted by priority)
    for addon in &sorted_addons {
        for imp in &addon.code_fragments.imports {
            if !imp.is_empty() && !imports.contains(&imp.to_string()) {
                imports.push(imp.to_string());
            }
        }
    }

    // When nothing serves (no server addon, template doesn't start its own
    // server), run the agent in the interactive console so `cargo run` does
    // something useful out of the box.
    const SELF_SERVING_TEMPLATES: &[&str] = &["api"];
    let has_server_addon = manifest.addons.iter().any(|a| a == "server");
    let interactive =
        !has_server_addon && !SELF_SERVING_TEMPLATES.contains(&manifest.template_name.as_str());
    if interactive {
        imports.push("use adk_rust::Launcher;".to_string());
    }

    let imports_section = imports.join("\n");

    // Build model initialization (includes api_key loading from env)
    // If model_override is set, replace the default model in the init code
    let model_init = if let Some(pc) = provider_config {
        let init_code = if let Some(ref model_id) = manifest.model_override {
            pc.model_init_code.replace(pc.default_model, model_id)
        } else {
            pc.model_init_code.to_string()
        };
        if pc.requires_api_key {
            format!(
                "    let api_key = std::env::var(\"{}\")\n        .map_err(|_| anyhow::anyhow!(\"{} is not set — copy .env.example to .env and add your key\"))?;\n    let model = {};",
                pc.env_var, pc.env_var, init_code
            )
        } else {
            format!("    let model = {};", init_code)
        }
    } else {
        let model_id = manifest.model_override.as_deref().unwrap_or("gemini-3.5-flash");
        format!(
            "    let api_key = std::env::var(\"GOOGLE_API_KEY\")\n        .map_err(|_| anyhow::anyhow!(\"GOOGLE_API_KEY is not set — copy .env.example to .env and add your key\"))?;\n    let model = adk_rust::model::GeminiModel::new(&api_key, \"{model_id}\")?;"
        )
    };

    // Build agent construction
    let agent_construction = if let Some(tmpl) = template {
        let code = tmpl.code_fragments.agent_construction;
        if code.is_empty() || code.starts_with("// TODO") {
            // Placeholder agent construction
            placeholder_agent_construction(project_name)
        } else {
            // Replace {name} placeholder with actual project name
            let resolved = code.replace("{name}", project_name);
            format!("    {resolved}")
        }
    } else {
        placeholder_agent_construction(project_name)
    };

    // Build addon initialization (sorted by priority)
    let mut addon_init_lines = Vec::new();
    for addon in &sorted_addons {
        let init = addon.code_fragments.initialization;
        if !init.is_empty() && !init.starts_with("// TODO") {
            addon_init_lines.push(format!("    {init}"));
        } else {
            // Generate placeholder initialization based on addon name
            addon_init_lines.push(format!(
                "    // {} initialization (priority {})",
                addon.name, addon.init_priority
            ));
            addon_init_lines.push(generate_placeholder_init(addon.name));
        }
    }

    // Build addon builder calls
    let mut builder_calls = Vec::new();
    for addon in &sorted_addons {
        let calls = addon.code_fragments.agent_builder_calls;
        if !calls.is_empty() {
            builder_calls.push(format!("    {calls}"));
        }
    }

    // Check if telemetry addon is present for tracing init
    let has_telemetry = manifest.addons.iter().any(|a| a == "telemetry");

    // Build tracing subscriber init
    let tracing_init = if has_telemetry {
        r#"    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(true)
        .init();
    tracing::info!("telemetry initialized");"#
            .to_string()
    } else {
        r#"    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();"#
            .to_string()
    };

    // Run the interactive console when nothing else drives the agent.
    // (When the server addon is present, its initialization binds and serves;
    // self-serving templates like `api` serve inside their construction code.)
    let launcher_section = if interactive {
        r#"
    // Interactive console
    Launcher::new(agent).run().await?;
"#
        .to_string()
    } else {
        String::new()
    };

    // Assemble the full main.rs
    let addon_init_section = if addon_init_lines.is_empty() {
        String::new()
    } else {
        format!(
            "\n    // Addon initialization (sorted by priority)\n{}\n",
            addon_init_lines.join("\n")
        )
    };

    let builder_calls_section = if builder_calls.is_empty() {
        String::new()
    } else {
        format!("\n    // Addon builder calls\n{}\n", builder_calls.join("\n"))
    };

    format!(
        r#"{imports_section}

#[tokio::main]
async fn main() -> anyhow::Result<()> {{
    dotenvy::dotenv().ok();

{tracing_init}

    // Provider model initialization
{model_init}

    // Agent construction
{agent_construction}
{addon_init_section}{builder_calls_section}{launcher_section}
    Ok(())
}}
"#
    )
}

/// Generate `.env.example` listing all required environment variables.
///
/// Collects env vars from the provider (if it requires an API key) and all addons.
pub fn generate_env_example(manifest: &CompositionManifest) -> String {
    let mut output = String::from("# Environment variables for this ADK-Rust agent project\n\n");

    if manifest.env_vars.is_empty() {
        output.push_str("# No environment variables required for this configuration.\n");
    } else {
        for (key, description) in &manifest.env_vars {
            output.push_str(&format!("# {description}\n"));
            output.push_str(&format!("{key}=\n\n"));
        }
    }

    // Always include RUST_LOG for tracing
    output.push_str("# Logging level (trace, debug, info, warn, error)\n");
    output.push_str("RUST_LOG=info\n");

    output
}

/// Generate `README.md` with template-specific documentation.
///
/// Includes project description, setup instructions, architecture overview,
/// and how to extend the agent.
pub fn generate_readme(manifest: &CompositionManifest, project_name: &str) -> String {
    let registry = TemplateRegistry::builtin();
    let template = registry.resolve_template(&manifest.template_name);

    let template_description = template.map(|t| t.description).unwrap_or("ADK-Rust agent project");

    let addons_section = if manifest.addons.is_empty() {
        String::new()
    } else {
        let addon_list: Vec<String> = manifest
            .addons
            .iter()
            .filter_map(|name| {
                registry
                    .capability_addons
                    .iter()
                    .find(|a| a.name == *name)
                    .map(|a| format!("- **{}**: {}", a.name, a.description))
            })
            .collect();
        format!("\n## Capabilities\n\n{}\n", addon_list.join("\n"))
    };

    let features_list: Vec<&str> = manifest.feature_set.iter().map(|s| s.as_str()).collect();

    let env_vars_section = if manifest.env_vars.is_empty() {
        String::new()
    } else {
        let vars: Vec<String> =
            manifest.env_vars.iter().map(|(key, desc)| format!("| `{key}` | {desc} |")).collect();
        format!(
            "\n## Environment Variables\n\n| Variable | Description |\n|----------|-------------|\n{}\n",
            vars.join("\n")
        )
    };

    format!(
        r#"# {project_name}

{template_description}

## Quick Start

```bash
# Install dependencies
cargo build

# Copy environment template
cp .env.example .env
# Edit .env with your API keys

# Run the agent
cargo run
```

## Architecture

- **Template**: `{template_name}` ({template_description})
- **Provider**: `{provider}`
- **Features**: `[{features}]`
{addons_section}{env_vars_section}
## Development

```bash
# Run with debug logging
RUST_LOG=debug cargo run

# Check for issues
cargo clippy -- -D warnings

# Format code
cargo fmt
```

## Extending

- Add tools by implementing the `Tool` trait
- Modify the agent instruction in `src/main.rs`
- Add new dependencies to `Cargo.toml`

## Resources

- [ADK-Rust Documentation](https://docs.rs/adk-rust)
- [ADK-Rust GitHub](https://github.com/zavora-ai/adk-rust)
"#,
        template_name = manifest.template_name,
        provider = manifest.provider,
        features = features_list.join(", "),
    )
}

/// Generate a standard Rust `.gitignore` file.
pub fn generate_gitignore() -> String {
    r#"/target
.env
*.swp
*.swo
*~
.DS_Store
"#
    .to_string()
}

/// Generate placeholder initialization code for an addon.
fn generate_placeholder_init(addon_name: &str) -> String {
    match addon_name {
        "telemetry" => "    tracing::info!(\"telemetry configured\");".to_string(),
        "auth" => "    tracing::info!(\"auth middleware configured\");".to_string(),
        "sessions" => "    tracing::info!(\"session service initialized\");".to_string(),
        "memory" => "    tracing::info!(\"memory service initialized\");".to_string(),
        "mcp" => "    tracing::info!(\"MCP tools connected\");".to_string(),
        "guardrails" => "    tracing::info!(\"guardrails configured\");".to_string(),
        "eval" => "    tracing::info!(\"eval harness ready\");".to_string(),
        "browser" => "    tracing::info!(\"browser tools initialized\");".to_string(),
        "server" => r#"    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    tracing::info!("server will bind to port {}", port);"#
            .to_string(),
        _ => format!("    tracing::info!(\"{addon_name} initialized\");"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::resolve_composition;
    use crate::registry::TemplateRegistry;

    fn registry() -> TemplateRegistry {
        TemplateRegistry::builtin()
    }

    #[test]
    fn test_generate_cargo_toml_basic() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &[], "gemini").unwrap();
        let toml = generate_cargo_toml(&manifest, "my-agent");

        assert!(toml.contains("name = \"my-agent\""));
        assert!(toml.contains("edition = \"2024\""));
        assert!(toml.contains(&format!("version = \"{ADK_VERSION}\"")));
        assert!(toml.contains("\"minimal\""));
        assert!(toml.contains("\"gemini\""));
        assert!(toml.contains("tokio"));
        assert!(toml.contains("dotenvy"));
        assert!(toml.contains("tracing"));
        assert!(toml.contains("anyhow"));
    }

    #[test]
    fn test_generate_cargo_toml_with_addons() {
        let reg = registry();
        let manifest =
            resolve_composition(&reg, "llm", &["telemetry", "sessions"], "openai").unwrap();
        let toml = generate_cargo_toml(&manifest, "my-agent");

        assert!(toml.contains("\"minimal\""));
        assert!(toml.contains("\"openai\""));
        assert!(toml.contains("\"telemetry\""));
        assert!(toml.contains("\"sessions\""));
    }

    #[test]
    fn test_generate_main_rs_basic() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &[], "gemini").unwrap();
        let main_rs = generate_main_rs(&manifest, "my-agent");

        assert!(main_rs.contains("use adk_rust::prelude::*;"));
        assert!(main_rs.contains("#[tokio::main]"));
        assert!(main_rs.contains("dotenvy::dotenv().ok();"));
        assert!(main_rs.contains("tracing_subscriber::fmt()"));
        assert!(main_rs.contains("GeminiModel::new(&api_key, \"gemini-3.5-flash\")?"));
        assert!(main_rs.contains("anyhow::Result<()>"));
    }

    #[test]
    fn test_generate_main_rs_with_server_addon_uses_port_env() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &["server"], "gemini").unwrap();
        let main_rs = generate_main_rs(&manifest, "my-agent");

        // Critical enterprise requirement: PORT env var
        assert!(
            main_rs.contains(r#"std::env::var("PORT").unwrap_or_else(|_| "8080".to_string())"#),
            "Server addon MUST use std::env::var(\"PORT\").unwrap_or_else(|_| \"8080\".to_string())"
        );
    }

    #[test]
    fn test_generate_main_rs_with_openai_provider() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &[], "openai").unwrap();
        let main_rs = generate_main_rs(&manifest, "my-agent");

        assert!(main_rs.contains("OpenAIClient::new("));
    }

    #[test]
    fn test_generate_main_rs_addon_ordering() {
        let reg = registry();
        // Pass addons in reverse priority order
        let manifest =
            resolve_composition(&reg, "llm", &["server", "telemetry", "auth"], "gemini").unwrap();
        let main_rs = generate_main_rs(&manifest, "my-agent");

        // Check initialization ordering within the addon initialization section.
        // Telemetry (10) should appear before auth (20) which should appear before server (90).
        // Use the initialization-specific markers to avoid matching import lines.
        let telemetry_init_pos = main_rs.find("telemetry initialized").or_else(|| {
            // Telemetry addon has empty initialization; codegen generates a placeholder comment
            main_rs.find("// telemetry initialization")
        });
        let auth_init_pos = main_rs.find("AUTH_API_KEY");
        let server_init_pos =
            main_rs.find(r#"std::env::var("PORT").unwrap_or_else(|_| "8080".to_string())"#);

        // Auth initialization must appear before server initialization
        if let (Some(auth_p), Some(server_p)) = (auth_init_pos, server_init_pos) {
            assert!(
                auth_p < server_p,
                "auth initialization should appear before server initialization"
            );
        }

        // If telemetry has a placeholder, it should appear before auth
        if let (Some(tel_p), Some(auth_p)) = (telemetry_init_pos, auth_init_pos) {
            assert!(
                tel_p < auth_p,
                "telemetry initialization should appear before auth initialization"
            );
        }
    }

    #[test]
    fn test_generate_env_example_with_provider() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &[], "openai").unwrap();
        let env = generate_env_example(&manifest);

        assert!(env.contains("OPENAI_API_KEY"));
        assert!(env.contains("RUST_LOG"));
    }

    #[test]
    fn test_generate_env_example_ollama_no_key() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &[], "ollama").unwrap();
        let env = generate_env_example(&manifest);

        // Ollama doesn't require an API key
        assert!(!env.contains("OLLAMA"));
        assert!(env.contains("RUST_LOG"));
    }

    #[test]
    fn test_generate_readme_basic() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &[], "gemini").unwrap();
        let readme = generate_readme(&manifest, "my-agent");

        assert!(readme.contains("# my-agent"));
        assert!(readme.contains("gemini"));
        assert!(readme.contains("cargo run"));
        assert!(readme.contains("cargo build"));
    }

    #[test]
    fn test_generate_readme_with_addons() {
        let reg = registry();
        let manifest =
            resolve_composition(&reg, "llm", &["telemetry", "sessions"], "gemini").unwrap();
        let readme = generate_readme(&manifest, "my-agent");

        assert!(readme.contains("Capabilities"));
        assert!(readme.contains("telemetry"));
        assert!(readme.contains("sessions"));
    }

    #[test]
    fn test_generate_gitignore() {
        let gitignore = generate_gitignore();

        assert!(gitignore.contains("/target"));
        assert!(gitignore.contains(".env"));
    }

    #[test]
    fn test_generate_project_produces_all_files() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &[], "gemini").unwrap();
        let files = generate_project(&manifest, "my-agent");

        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"Cargo.toml"));
        assert!(paths.contains(&"src/main.rs"));
        assert!(paths.contains(&".env.example"));
        assert!(paths.contains(&"README.md"));
        assert!(paths.contains(&".gitignore"));
    }

    #[test]
    fn test_generate_cargo_toml_edition_2024() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &[], "gemini").unwrap();
        let toml = generate_cargo_toml(&manifest, "test-project");

        assert!(toml.contains("edition = \"2024\""));
    }

    #[test]
    fn test_generate_cargo_toml_adk_version() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &[], "gemini").unwrap();
        let toml = generate_cargo_toml(&manifest, "test-project");

        // Should contain the current ADK version
        assert!(toml.contains(ADK_VERSION), "Cargo.toml should contain ADK_VERSION: {ADK_VERSION}");
    }

    #[test]
    fn test_generate_main_rs_all_providers() {
        let reg = registry();
        let providers = [
            "gemini",
            "openai",
            "anthropic",
            "deepseek",
            "ollama",
            "groq",
            "openrouter",
            "bedrock",
            "azure-ai",
        ];

        for provider in providers {
            let manifest = resolve_composition(&reg, "llm", &[], provider).unwrap();
            let main_rs = generate_main_rs(&manifest, "test-project");

            // All should produce valid-looking code
            assert!(
                main_rs.contains("#[tokio::main]"),
                "Provider {provider} should produce tokio::main"
            );
            assert!(
                main_rs.contains("dotenvy::dotenv().ok()"),
                "Provider {provider} should include dotenvy"
            );
        }
    }

    #[test]
    fn test_generate_main_rs_with_multiple_addons() {
        let reg = registry();
        let manifest = resolve_composition(
            &reg,
            "llm",
            &["telemetry", "sessions", "memory", "server"],
            "gemini",
        )
        .unwrap();
        let main_rs = generate_main_rs(&manifest, "my-agent");

        // Should contain initialization for all addons
        assert!(main_rs.contains("telemetry"));
        assert!(main_rs.contains("session"));
        assert!(main_rs.contains("memory"));
        assert!(main_rs.contains("server"));
        // Server must use PORT env var
        assert!(main_rs.contains(r#"std::env::var("PORT")"#));
    }
}
