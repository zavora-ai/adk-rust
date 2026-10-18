//! Validates artifact versioning and naming, evaluation verdicts, and memory search on the
//! real stack: an `LlmAgent` and `Runner` on a live model saving and loading artifacts
//! through tools, an `adk-eval` run graded by a live judge, and memory recall through a tool.
//!
//! Faults a live model cannot trigger on demand use scripted pieces instead: a direct
//! concurrent harness, a writer process killed mid-save, a scripted GCS JSON API on
//! 127.0.0.1, and hand-written legacy directory layouts. Each row says which.
//!
//! ```bash
//! ADK_ENV_FILE=/path/to/.env \
//!   cargo run --manifest-path examples/artifacts_eval_memory/Cargo.toml -- --provider gemini
//! ```

mod artifacts;
mod common;
mod eval;
mod gcs_server;
mod memory;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use adk_core::Llm;
use common::{Mode, Provider, Verdict};

const SCENARIO_TIMEOUT: Duration = Duration::from_secs(300);
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(60);

/// Every scenario: name, finding, and whether it calls a live model.
const SCENARIOS: &[(&str, &str, Mode)] = &[
    ("artifact-parallel-saves-file", "SS8", Mode::Live),
    ("artifact-parallel-saves-gcs", "SS8", Mode::Live),
    ("artifact-concurrent-saves", "SS8", Mode::Scripted),
    ("artifact-name-collision", "SS10", Mode::Live),
    ("artifact-legacy-layout", "SS10", Mode::Scripted),
    ("artifact-crash-mid-write", "SS18", Mode::Scripted),
    ("artifact-reserved-session", "SS21", Mode::Live),
    ("eval-live-judged-case", "SS9", Mode::Live),
    ("eval-zero-cases", "SS9", Mode::Live),
    ("eval-zero-turns", "SS9", Mode::Live),
    ("eval-no-criteria", "SS9", Mode::Live),
    ("eval-nothing-judged", "SS9", Mode::Live),
    ("eval-zero-concurrency", "SS9", Mode::Live),
    ("memory-punctuation-recall", "SS20", Mode::Live),
    ("memory-ranked-top-k", "SS20", Mode::Scripted),
    ("memory-image-only-entry", "SS20", Mode::Scripted),
];

async fn run(
    name: &str,
    model: Option<Arc<dyn Llm>>,
    judge: Option<Arc<dyn Llm>>,
) -> anyhow::Result<Verdict> {
    let live = || model.clone().ok_or_else(|| anyhow::anyhow!("no live model"));
    let judge = || judge.clone().ok_or_else(|| anyhow::anyhow!("no live judge"));
    match name {
        "artifact-parallel-saves-file" => {
            artifacts::parallel_saves(live()?, artifacts::Backend::file().await?).await
        }
        "artifact-parallel-saves-gcs" => {
            artifacts::parallel_saves(live()?, artifacts::Backend::gcs().await?).await
        }
        "artifact-concurrent-saves" => artifacts::concurrent_saves().await,
        "artifact-name-collision" => artifacts::name_collision(live()?).await,
        "artifact-legacy-layout" => artifacts::legacy_layout().await,
        "artifact-crash-mid-write" => artifacts::crash_mid_write().await,
        "artifact-reserved-session" => artifacts::reserved_session(live()?).await,
        "eval-live-judged-case" => eval::live_judged_case(live()?, judge()?).await,
        "eval-zero-cases" => eval::zero_cases(live()?, judge()?).await,
        "eval-zero-turns" => eval::zero_turns(live()?, judge()?).await,
        "eval-no-criteria" => eval::no_criteria(live()?, judge()?).await,
        "eval-nothing-judged" => eval::nothing_judged(live()?, judge()?).await,
        "eval-zero-concurrency" => eval::zero_concurrency(live()?, judge()?).await,
        "memory-punctuation-recall" => memory::punctuation_recall(live()?).await,
        "memory-ranked-top-k" => memory::ranked_top_k().await,
        "memory-image-only-entry" => memory::image_only_entry().await,
        other => Err(anyhow::anyhow!("unknown scenario {other}")),
    }
}

async fn run_once(
    name: &str,
    model: &Option<Arc<dyn Llm>>,
    judge: &Option<Arc<dyn Llm>>,
) -> Verdict {
    match tokio::time::timeout(SCENARIO_TIMEOUT, run(name, model.clone(), judge.clone())).await {
        Ok(Ok(verdict)) => verdict,
        Ok(Err(error)) => Verdict::Fail(format!("scenario error: {error:#}")),
        Err(_) => Verdict::Fail(format!("timed out after {}s", SCENARIO_TIMEOUT.as_secs())),
    }
}

fn parse_provider(value: &str) -> Result<Provider, String> {
    match value {
        "gemini" => Ok(Provider::Gemini),
        "openai" => Ok(Provider::OpenAi),
        "anthropic" => Ok(Provider::Anthropic),
        other => Err(format!("unknown provider {other}; use gemini, openai or anthropic")),
    }
}

struct Args {
    provider: Provider,
    judge: Option<Provider>,
    scenarios: Vec<&'static str>,
}

fn parse_args() -> Result<Args, String> {
    let mut parsed = Args {
        provider: Provider::Gemini,
        judge: None,
        scenarios: SCENARIOS.iter().map(|s| s.0).collect(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--provider" => parsed.provider = parse_provider(&value)?,
            "--judge" => parsed.judge = Some(parse_provider(&value)?),
            "--scenario" if value == "all" => {}
            "--scenario" => {
                parsed.scenarios = value
                    .split(',')
                    .map(|name| {
                        SCENARIOS
                            .iter()
                            .find(|known| known.0 == name)
                            .map(|known| known.0)
                            .ok_or_else(|| format!("unknown scenario {name}"))
                    })
                    .collect::<Result<_, _>>()?;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    Ok(parsed)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 8)]
async fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("--crash-writer") {
        return match artifacts::crash_writer(&argv[2]).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("crash writer: {error}");
                ExitCode::from(3)
            }
        };
    }

    match std::env::var("ADK_ENV_FILE") {
        // The named file wins over variables already in the environment.
        Ok(path) => {
            if let Err(error) = dotenvy::from_path_override(&path) {
                eprintln!("could not load ADK_ENV_FILE: {error}");
                return ExitCode::from(2);
            }
        }
        Err(_) => {
            let _ = dotenvy::dotenv();
        }
    }
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let judge_provider = args.judge.unwrap_or(args.provider);
    let (model, judge) = match (args.provider.model(), judge_provider.model()) {
        (Ok(model), Ok(judge)) => (model, judge),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("could not build a model: {error}");
            return ExitCode::from(2);
        }
    };
    println!("agent model: {}; judge model: {}\n", args.provider.label(), judge_provider.label());

    let mut failed = 0;
    for name in &args.scenarios {
        let (_, finding, mode) =
            SCENARIOS.iter().find(|known| known.0 == *name).copied().expect("known scenario");
        let verdict = if matches!(mode, Mode::Live) && model.is_none() {
            Verdict::Skip(format!("no API key for {}", args.provider.label()))
        } else {
            match run_once(name, &model, &judge).await {
                // A provider rate limit is not a finding; wait it out once.
                Verdict::Fail(evidence) if evidence.contains("rate_limited") => {
                    tokio::time::sleep(RATE_LIMIT_BACKOFF).await;
                    match run_once(name, &model, &judge).await {
                        Verdict::Pass(evidence) => {
                            Verdict::Pass(format!("{evidence} [retried once after HTTP 429]"))
                        }
                        Verdict::Fail(evidence) => {
                            Verdict::Fail(format!("{evidence} [retried once after HTTP 429]"))
                        }
                        skip => skip,
                    }
                }
                other => other,
            }
        };
        let (status, evidence) = match verdict {
            Verdict::Pass(evidence) => ("PASS", evidence),
            Verdict::Fail(evidence) => {
                failed += 1;
                ("FAIL", evidence)
            }
            Verdict::Skip(evidence) => ("SKIP", evidence),
        };
        println!("{status} [{finding}, {}] {name}: {evidence}", mode.label());
    }
    println!("\n{failed} scenario(s) failed");
    if failed == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
