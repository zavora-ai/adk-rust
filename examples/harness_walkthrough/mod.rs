//! The harness walkthrough, shared by `examples/harness_openai` and
//! `examples/harness_anthropic`: each crate supplies its provider's model and calls [`main`].
//!
//! ```text
//! cargo run                          # every scenario, scripted (no key)
//! cargo run -- --live                # every scenario on the provider's model
//! cargo run -- --live approvals      # one scenario
//! cargo run -- --live --model <id>   # another model of the provider
//! ```

mod scenarios;
mod support;

use std::sync::Arc;

use adk_core::Llm;
use anyhow::{Result, bail};

use scenarios::SCENARIOS;
use support::Models;

/// The provider a walkthrough crate runs on.
pub struct Provider {
    /// The provider's name in the report.
    pub name: &'static str,
    /// The environment variable holding the API key.
    pub key_var: &'static str,
    /// The environment variable that overrides the model.
    pub model_var: &'static str,
    /// The model the walkthrough uses by default.
    pub default_model: &'static str,
    /// Builds the model from an API key and a model id.
    pub build: fn(&str, &str) -> Result<Arc<dyn Llm>>,
}

/// Loads keys from `HARNESS_ENV_FILE`, or from `.env` in the working directory.
fn load_env() {
    match std::env::var("HARNESS_ENV_FILE") {
        Ok(path) => {
            dotenvy::from_path_override(path).ok();
        }
        Err(_) => {
            dotenvy::dotenv().ok();
        }
    }
}

/// Runs the walkthrough on `provider`: scripted by default, live with `--live`.
pub async fn main(provider: Provider) -> Result<()> {
    load_env();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let live = args.iter().any(|arg| arg == "--live");
    let model_flag =
        args.iter().position(|arg| arg == "--model").and_then(|at| args.get(at + 1)).cloned();
    let selected: Vec<&str> = args
        .iter()
        .filter(|arg| !arg.starts_with("--") && Some(*arg) != model_flag.as_ref())
        .map(String::as_str)
        .collect();
    if let Some(unknown) = selected.iter().find(|name| !SCENARIOS.iter().any(|s| s.name == **name))
    {
        bail!(
            "unknown scenario {unknown}; one of: {}",
            SCENARIOS.iter().map(|s| s.name).collect::<Vec<_>>().join(", ")
        );
    }

    let models = if live {
        let key = std::env::var(provider.key_var).ok().filter(|key| !key.trim().is_empty());
        let Some(key) = key else {
            bail!(
                "--live needs {} (in the environment, .env, or the file HARNESS_ENV_FILE names)",
                provider.key_var
            );
        };
        let model = model_flag
            .or_else(|| std::env::var(provider.model_var).ok().filter(|model| !model.is_empty()))
            .unwrap_or_else(|| provider.default_model.to_string());
        Models::Live {
            label: format!("{}/{model}", provider.name),
            model: (provider.build)(&key, &model)?,
        }
    } else {
        Models::Scripted
    };

    println!("adk-harness walkthrough on {} ({})\n", provider.name, models.label());
    let mut rows = Vec::new();
    let mut failed = false;
    for scenario in SCENARIOS {
        if !selected.is_empty() && !selected.contains(&scenario.name) {
            continue;
        }
        println!("== {} ({}): {}", scenario.name, scenario.spec, scenario.title);
        let result = scenarios::run(scenario.name, &models).await;
        let verdict = match &result {
            Ok(checks) => match checks.skip_reason() {
                Some(reason) => format!("SKIPPED: {reason}"),
                None if checks.passed() => {
                    let (passed, total) = checks.tally();
                    format!("PASS ({passed}/{total} checks)")
                }
                None => format!("FAIL: {}", checks.failures().join("; ")),
            },
            Err(error) => format!("ERROR: {error:#}"),
        };
        failed |= verdict.starts_with("FAIL") || verdict.starts_with("ERROR");
        println!("  => {verdict}\n");
        rows.push(format!(
            "| `{}` | {} | {} | {verdict} |",
            scenario.name,
            scenario.spec,
            models.label()
        ));
    }
    println!("| Scenario | Spec 3.1 | Model | Result |\n|---|---|---|---|");
    for row in rows {
        println!("{row}");
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}
