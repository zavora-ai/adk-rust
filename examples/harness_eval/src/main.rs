//! The adk-harness eval suite (spec 3.1 §7.13) on a scripted or a live model.
//!
//! ```bash
//! cargo run                         # scripted model, checked against the adk-harness baseline
//! cargo run -- --live openai        # gpt-5.6-terra
//! cargo run -- --live anthropic     # claude-sonnet-5-5
//! cargo run -- --live gemini        # gemini-3.5-flash-lite
//! cargo run -- --live openai --save-baseline
//! ```
//!
//! A live run is compared on the `passed` metric against `baseline.live.<provider>.json`;
//! the process exits non-zero on a regression, which is what the nightly job checks. The
//! capability questions (spec 3.1 §7.25) join the `passed` metric under their own ids.

#[path = "../../../adk-harness/tests/eval/cases.rs"]
mod cases;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use adk_core::Llm;
use adk_eval::BaselineStore;
use anyhow::{Context, bail};

fn live_model(provider: &str) -> anyhow::Result<Arc<dyn Llm>> {
    let key = |name: &str| std::env::var(name).with_context(|| format!("{name} is not set"));
    Ok(match provider {
        "openai" => Arc::new(adk_model::openai::OpenAIClient::new(
            adk_model::openai::OpenAIConfig::new(key("OPENAI_API_KEY")?, "gpt-5.6-terra"),
        )?),
        "anthropic" => Arc::new(adk_model::anthropic::AnthropicClient::new(
            adk_model::anthropic::AnthropicConfig::new(key("ANTHROPIC_API_KEY")?, "claude-sonnet-5-5"),
        )?),
        "gemini" => Arc::new(adk_model::gemini::GeminiModel::new(
            key("GOOGLE_API_KEY")?,
            "gemini-3.5-flash-lite",
        )?),
        other => bail!("unknown provider '{other}': use openai, anthropic or gemini"),
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| args.iter().position(|arg| arg == name);
    let value = |name: &str| flag(name).and_then(|i| args.get(i + 1)).cloned();
    match value("--env-file") {
        Some(path) => {
            dotenvy::from_path_override(&path).with_context(|| format!("load {path}"))?;
        }
        None => {
            dotenvy::dotenv().ok();
        }
    }
    let live = value("--live");
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let (baseline, compared): (PathBuf, &[&str]) = match &live {
        Some(provider) => (here.join(format!("baseline.live.{provider}.json")), &["passed"]),
        None => (
            here.join("../../adk-harness/tests/eval/baseline.scripted.json"),
            &["passed", "trajectory", "efficiency"],
        ),
    };

    let scenarios = cases::scenarios();
    let mut outcomes = Vec::new();
    for scenario in &scenarios {
        let model = match &live {
            Some(provider) => live_model(provider)?,
            None => cases::scripted_model(scenario),
        };
        let outcome = cases::run(scenario, model).await;
        println!(
            "{:<26} {}  turns={} tools=[{}]",
            outcome.id,
            if outcome.passed { "PASS" } else { "FAIL" },
            outcome.iterations,
            outcome.tool_calls.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>().join(", ")
        );
        outcomes.push(outcome);
    }
    let mut metrics: HashMap<String, HashMap<String, f64>> = cases::metrics(&scenarios, &outcomes)
        .into_iter()
        .filter(|(metric, _)| compared.contains(&metric.as_str()))
        .collect();
    // The rubric case (spec 3.1 §7.25): questions about the harness itself, scored on
    // whether the answer names the commands the `<harness>` block lists.
    for question in cases::capability_questions() {
        let model = match &live {
            Some(provider) => live_model(provider)?,
            None => cases::capability_model(&question),
        };
        let outcome = cases::ask(&question, model).await;
        println!("{:<26} {}", outcome.id, if outcome.passed { "PASS" } else { "FAIL" });
        metrics
            .entry("passed".to_string())
            .or_default()
            .insert(outcome.id, if outcome.passed { 1.0 } else { 0.0 });
    }

    let store = BaselineStore::new(&baseline);
    if flag("--save-baseline").is_some() {
        store.save(cases::EVAL_SET_ID, &metrics)?;
        println!("baseline saved to {}", baseline.display());
        return Ok(());
    }
    let regressions = store.check_regressions(&metrics, 0.0)?;
    if regressions.is_empty() {
        println!("no regressions against {}", baseline.display());
        return Ok(());
    }
    for regression in &regressions {
        println!(
            "REGRESSION {} {}: {} -> {:?}",
            regression.metric_name,
            regression.case_id,
            regression.baseline_value,
            regression.current_value
        );
    }
    bail!("{} regressions against {}", regressions.len(), baseline.display())
}
