//! Live validation of the autonomy-readiness Phase 0 and Phase 1 fixes against OpenAI,
//! Anthropic, and Gemini.
//!
//! Each scenario drives real model calls through the public ADK APIs and asserts on observable
//! facts — session history, tool execution counters, persisted state, checkpoints, and usage —
//! never on model prose. A scenario whose model ignores a directive prompt is retried once and
//! the retry is reported.
//!
//! ```bash
//! cargo run --manifest-path examples/autonomy_validation/Cargo.toml -- --provider all --scenario all
//! ```

mod common;
mod scenarios;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use common::{API_CALLS, Config, Provider, Verdict, brief};
use tracing_subscriber::EnvFilter;

const SCENARIO_TIMEOUT: Duration = Duration::from_secs(600);
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(60);

struct Row {
    scenario: &'static str,
    provider: Provider,
    status: &'static str,
    evidence: String,
}

fn usage() -> String {
    format!(
        "usage: autonomy-validation [--provider openai|openai-chat|anthropic|gemini|both|all] [--scenario <name>[,<name>]|all]\n\
         openai uses the Responses API; openai-chat uses Chat Completions; both = openai + anthropic;\n\
         all = openai + openai-chat + anthropic + gemini\n\
         scenarios: {}",
        scenarios::ALL.join(", ")
    )
}

fn parse_args() -> Result<(Vec<Provider>, Vec<&'static str>), String> {
    let mut providers = vec![Provider::OpenAi, Provider::Anthropic];
    let mut selected: Vec<&'static str> = scenarios::ALL.to_vec();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value\n{}", usage()))?;
        match flag.as_str() {
            "--provider" => {
                providers = match value.as_str() {
                    "openai" => vec![Provider::OpenAi],
                    "openai-chat" => vec![Provider::OpenAiChat],
                    "anthropic" => vec![Provider::Anthropic],
                    "gemini" => vec![Provider::Gemini],
                    "both" => vec![Provider::OpenAi, Provider::Anthropic],
                    "all" => vec![
                        Provider::OpenAi,
                        Provider::OpenAiChat,
                        Provider::Anthropic,
                        Provider::Gemini,
                    ],
                    other => return Err(format!("unknown provider {other}\n{}", usage())),
                }
            }
            "--scenario" => {
                selected = if value == "all" {
                    scenarios::ALL.to_vec()
                } else {
                    let names: Vec<&str> = value.split(',').collect();
                    let mut chosen = Vec::new();
                    for name in names {
                        let known = scenarios::ALL
                            .iter()
                            .find(|known| **known == name)
                            .ok_or_else(|| format!("unknown scenario {name}\n{}", usage()))?;
                        chosen.push(*known);
                    }
                    chosen
                }
            }
            other => return Err(format!("unknown flag {other}\n{}", usage())),
        }
    }
    Ok((providers, selected))
}

async fn run_once(name: &str, cfg: &Config, provider: Provider) -> Verdict {
    match tokio::time::timeout(SCENARIO_TIMEOUT, scenarios::run(name, cfg, provider)).await {
        Ok(verdict) => verdict,
        Err(_) => Verdict::Fail(format!("timed out after {}s", SCENARIO_TIMEOUT.as_secs())),
    }
}

/// Runs a scenario, retrying once when the model ignored a directive prompt.
async fn run_with_retry(name: &'static str, cfg: &Config, provider: Provider) -> Row {
    let row = |status, evidence| Row { scenario: name, provider, status, evidence };
    if !cfg.has_key(provider) {
        return row("SKIP", format!("no {} API key", provider.label()));
    }
    let first = run_once(name, cfg, provider).await;
    let (verdict, retried) = match first {
        Verdict::Retry(reason) => {
            tracing::warn!(scenario = name, provider = provider.label(), reason = %reason, "retrying after model non-compliance");
            (run_once(name, cfg, provider).await, Some(reason))
        }
        // A provider quota or rate limit is not a finding; wait it out once.
        Verdict::Fail(evidence) if evidence.contains("model.rate_limited") => {
            tracing::warn!(
                scenario = name,
                provider = provider.label(),
                "retrying after a provider rate limit"
            );
            tokio::time::sleep(RATE_LIMIT_BACKOFF).await;
            (
                run_once(name, cfg, provider).await,
                Some("provider rate limit (HTTP 429)".to_string()),
            )
        }
        other => (other, None),
    };
    let note = |evidence: String| match &retried {
        Some(reason) => format!("{evidence} [retried once: {}]", brief(reason)),
        None => evidence,
    };
    match verdict {
        Verdict::Pass(evidence) => row("PASS", note(evidence)),
        Verdict::Fail(evidence) => row("FAIL", note(evidence)),
        Verdict::Skip(evidence) => row("SKIP", note(evidence)),
        Verdict::Retry(reason) => row("FAIL", note(format!("model non-compliance: {reason}"))),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match std::env::var("ADK_ENV_FILE") {
        // The named file wins over variables already in the environment.
        Ok(path) => {
            if let Err(error) = dotenvy::from_path_override(&path) {
                eprintln!("could not load ADK_ENV_FILE: {error}");
                return ExitCode::from(2);
            }
        }
        Err(_) => {
            dotenvy::dotenv().ok();
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("error")),
        )
        .with_writer(std::io::stderr)
        .init();
    adk_core::ensure_crypto_provider();

    let (providers, selected) = match parse_args() {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let cfg = Config::from_env();

    println!("════════════════════════════════════════════════════════════════");
    println!(" ADK-Rust autonomy validation — Phase 0 and Phase 1 against live models");
    println!("════════════════════════════════════════════════════════════════");
    for provider in &providers {
        let model = match provider {
            Provider::OpenAi => format!("{} (Responses API)", cfg.openai_model),
            Provider::OpenAiChat => format!("{} (Chat Completions)", cfg.openai_chat_model),
            Provider::Anthropic => format!(
                "{} (web search: {}, thinking: {})",
                cfg.anthropic_model, cfg.anthropic_web_model, cfg.anthropic_thinking_model
            ),
            Provider::Gemini => cfg.gemini_model.clone(),
        };
        println!(" {:<12} {model}", provider.label());
    }
    println!();

    let started = Instant::now();
    let mut rows = Vec::new();
    for scenario in selected {
        for provider in &providers {
            let began = Instant::now();
            let row = run_with_retry(scenario, &cfg, *provider).await;
            println!(
                "[{:>4}] {:<30} {:<12} {:>5.1}s  {}",
                row.status,
                row.scenario,
                row.provider.label(),
                began.elapsed().as_secs_f64(),
                row.evidence
            );
            rows.push(row);
        }
    }

    println!();
    println!("{:<30} {:<12} {:<6} EVIDENCE", "SCENARIO", "PROVIDER", "RESULT");
    println!("{}", "─".repeat(100));
    for row in &rows {
        println!(
            "{:<30} {:<12} {:<6} {}",
            row.scenario,
            row.provider.label(),
            row.status,
            brief(&row.evidence)
        );
    }
    let failed = rows.iter().filter(|row| row.status == "FAIL").count();
    println!(
        "\n{} passed, {failed} failed, {} skipped — {} model calls in {:.0}s",
        rows.iter().filter(|row| row.status == "PASS").count(),
        rows.iter().filter(|row| row.status == "SKIP").count(),
        API_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        started.elapsed().as_secs_f64()
    );
    if failed > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
