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
    // File contents honor the same `{name}` placeholder as agent construction
    // fragments — the docker addon needs the crate name to locate the release
    // binary inside the build stage.
    //
    // A fragment with the same path as an earlier file replaces it, so a
    // template can override a base file (the agent-engine template ships its
    // own README.md).
    let push_or_replace = |files: &mut Vec<GeneratedFile>, path: &str, content: &str| {
        let content = content.replace("{name}", project_name);
        match files.iter_mut().find(|f| f.path == path) {
            Some(existing) => existing.content = content,
            None => files.push(GeneratedFile { path: path.to_string(), content }),
        }
    };
    if let Some(template) = registry.resolve_template(&manifest.template_name) {
        for fragment in &template.code_fragments.additional_files {
            push_or_replace(&mut files, fragment.path, fragment.content);
        }
    }
    for addon_name in &manifest.addons {
        if let Some(addon) = registry.capability_addons.iter().find(|a| a.name == *addon_name) {
            for fragment in &addon.code_fragments.additional_files {
                push_or_replace(&mut files, fragment.path, fragment.content);
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

    // The agent-engine umbrella feature ships in the first release after the
    // published 2.0.0, so a crates.io resolution of the generated manifest
    // fails until then; the header comment states the workaround.
    // Raw string: scripts/check-doc-versions.py scans .rs sources for
    // `adk-rust = { ... features = [...] }` snippets and validates the
    // feature names; escaped quotes would corrupt what it extracts.
    let header = if features.contains(&"agent-engine") {
        r#"# The `agent-engine` feature requires the first adk-rust release after 2.0.0.
# Until it is published on crates.io, use the git repository instead:
#   adk-rust = { git = "https://github.com/zavora-ai/adk-rust", default-features = false, features = ["minimal", "agent-engine"] }

"#
    } else {
        ""
    };

    let mut output = format!(
        r#"{header}[package]
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
    const SELF_SERVING_TEMPLATES: &[&str] = &["api", "agent-engine"];
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
        let model_id =
            manifest.model_override.as_deref().unwrap_or(adk_model::catalog::GEMINI_DEFAULT);
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
        assert!(main_rs.contains("GeminiModel::new(&api_key, \"gemini-3.7-flash\")?"));
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
    fn test_generate_project_with_docker_addon_renders_container_files() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "llm", &["docker"], "gemini").unwrap();
        let files = generate_project(&manifest, "my-agent");

        let file = |path: &str| {
            files
                .iter()
                .find(|f| f.path == path)
                .map(|f| f.content.as_str())
                .unwrap_or_else(|| panic!("expected generated file '{path}'"))
        };

        let dockerfile = file("Dockerfile");
        assert!(dockerfile.contains("FROM rust:1.95-slim AS builder"));
        assert!(dockerfile.contains("rust-toolchain.toml"));
        assert!(dockerfile.contains("cargo build --release"));
        assert!(dockerfile.contains("FROM gcr.io/distroless/cc-debian12"));
        assert!(
            dockerfile.contains("COPY --from=builder /build/target/release/my-agent /app/agent")
        );
        assert!(dockerfile.contains("ENV PORT=8080"));
        assert!(dockerfile.contains(r#"ENTRYPOINT ["/app/agent"]"#));

        let static_dockerfile = file("Dockerfile.static");
        assert!(static_dockerfile.contains("FROM rust:1.95-slim AS builder"));
        assert!(static_dockerfile.contains("rustup target add x86_64-unknown-linux-musl"));
        assert!(static_dockerfile.contains("musl-tools cmake ca-certificates"));
        assert!(static_dockerfile.contains("FROM scratch"));
        // rustls-tls-native-roots reads /etc/ssl/certs at runtime, so the CA
        // bundle copy is load-bearing, not cosmetic.
        assert!(static_dockerfile.contains(
            "COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt"
        ));
        assert!(static_dockerfile.contains(
            "COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/my-agent /app/agent"
        ));
        assert!(static_dockerfile.contains("ENV PORT=8080"));
        assert!(static_dockerfile.contains(r#"ENTRYPOINT ["/app/agent"]"#));
        // The compatibility guard names the feature sets that cannot link statically.
        assert!(static_dockerfile.contains("gemini-agent-platform"));
        assert!(static_dockerfile.contains("livekit"));
        assert!(static_dockerfile.contains("onnx"));

        let dockerignore = file(".dockerignore");
        assert!(dockerignore.contains("target/"));
        assert!(dockerignore.contains(".git/"));
        assert!(dockerignore.contains(".env"));
    }

    #[test]
    fn test_generate_project_agent_engine_renders_deploy_files() {
        let reg = registry();
        let manifest = resolve_composition(&reg, "agent-engine", &[], "gemini").unwrap();
        let files = generate_project(&manifest, "my-agent");

        let file = |path: &str| {
            files
                .iter()
                .find(|f| f.path == path)
                .map(|f| f.content.as_str())
                .unwrap_or_else(|| panic!("expected generated file '{path}'"))
        };

        // main.rs serves the dispatch contract and nothing else drives it.
        let main_rs = file("src/main.rs");
        assert!(main_rs.contains(
            "use adk_rust::server::agent_engine::{AgentEngineOptions, serve_agent_engine};"
        ));
        assert!(main_rs.contains("serve_agent_engine(agent, AgentEngineOptions::new()).await?;"));
        assert!(main_rs.contains(r#"LlmAgentBuilder::new("my-agent")"#));
        assert!(!main_rs.contains("Launcher"), "self-serving template must not run the console");

        // Cargo.toml carries the features and the version-requirement comment.
        let cargo_toml = file("Cargo.toml");
        assert!(cargo_toml.contains("\"minimal\""));
        assert!(cargo_toml.contains("\"agent-engine\""));
        assert!(
            cargo_toml.contains("first adk-rust release after 2.0.0"),
            "Cargo.toml must state that the agent-engine feature is not in the published 2.0.0"
        );

        // The docker addon's Dockerfile, with the crate name substituted.
        let dockerfile = file("Dockerfile");
        assert!(dockerfile.contains("FROM rust:1.95-slim AS builder"));
        assert!(dockerfile.contains("FROM gcr.io/distroless/cc-debian12"));
        assert!(
            dockerfile.contains("COPY --from=builder /build/target/release/my-agent /app/agent")
        );
        assert!(dockerfile.contains("ENV PORT=8080"));
        assert!(dockerfile.contains(r#"ENTRYPOINT ["/app/agent"]"#));
        assert!(files.iter().any(|f| f.path == ".dockerignore"));
        assert!(
            !files.iter().any(|f| f.path == "Dockerfile.static"),
            "the static variant belongs to the docker addon, not this template"
        );

        // Terraform: BYOC resource, image variable, and all 14 class methods.
        let main_tf = file("deploy/terraform/main.tf");
        assert!(main_tf.contains(r#"resource "google_vertex_ai_reasoning_engine" "agent""#));
        assert!(main_tf.contains("image_uri = var.image_uri"));
        assert!(main_tf.contains("class_methods   = jsonencode(local.class_methods)"));
        assert!(main_tf.contains(r#"agent_framework = "google-adk""#));
        for method in [
            "create_session",
            "get_session",
            "list_sessions",
            "delete_session",
            "register_operations",
            "async_create_session",
            "async_get_session",
            "async_list_sessions",
            "async_delete_session",
            "async_add_session_to_memory",
            "async_search_memory",
            "stream_query",
            "async_stream_query",
            "streaming_agent_run_with_events",
        ] {
            assert!(
                main_tf.contains(&format!(r#""name" = "{method}""#)),
                "class_methods must declare '{method}'"
            );
        }

        let variables_tf = file("deploy/terraform/variables.tf");
        assert!(variables_tf.contains(r#"variable "project_id""#));
        assert!(variables_tf.contains(r#"variable "location""#));
        assert!(variables_tf.contains(r#"variable "image_uri""#));
        assert!(variables_tf.contains(r#"variable "display_name""#));
        assert!(variables_tf.contains(r#"variable "service_account""#));

        let outputs_tf = file("deploy/terraform/outputs.tf");
        assert!(outputs_tf.contains(r#"output "reasoning_engine_id""#));
        assert!(outputs_tf.contains(r#"output "reasoning_engine_name""#));

        // The template's README replaces the generic one (exactly one README).
        let readmes: Vec<_> = files.iter().filter(|f| f.path == "README.md").collect();
        assert_eq!(readmes.len(), 1);
        let readme = readmes[0].content.as_str();
        assert!(readme.contains("gcloud builds submit"));
        assert!(readme.contains("terraform -chdir=deploy/terraform apply"));
        assert!(readme.contains("GOOGLE_CLOUD_AGENT_ENGINE_ID"));
        assert!(readme.contains("/.well-known/agent.json"), "README documents the A2A option");
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
