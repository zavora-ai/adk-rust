//! Release-consistency checks for the 3.0 changelog audit.
//!
//! Each scenario drives the real stack — `LlmAgent` + `Runner` with a provider client, a
//! summarizer, an `Evaluator`, or the server router — against a live API, a scripted local
//! upstream, or in process, and asserts the documented behaviour.
//!
//! ```bash
//! cargo run            # every scenario
//! cargo run -- 1 3     # only these audit items
//! ```

mod common;
mod scenarios;
mod upstream;

use common::{Mode, Report, Verdict};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // The shell may export stale keys, so the file's values win.
    let env_file = std::env::var("ADK_ENV_FILE")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env").to_string());
    if dotenvy::from_path_override(&env_file).is_err() {
        let _ = dotenvy::dotenv_override();
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "error".into()),
        )
        .init();

    let items: Vec<u8> = std::env::args().skip(1).filter_map(|arg| arg.parse().ok()).collect();
    let selected = |item: u8| items.is_empty() || items.contains(&item);
    let gemini_key = ["GEMINI_API_KEY", "GOOGLE_API_KEY"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.trim().is_empty()));

    let mut report = Report::default();
    if selected(1) {
        scenarios::gemini::run(gemini_key.as_deref(), &mut report).await;
    }
    if selected(2) {
        scenarios::fields::run(&mut report).await;
    }
    if selected(3) {
        scenarios::timeouts::run(&mut report).await;
    }
    if selected(4) {
        scenarios::finish::run(&mut report).await;
    }
    if selected(5) {
        scenarios::responses_errors::run(&mut report).await;
    }
    if selected(6) {
        scenarios::compaction::run(&mut report).await;
    }
    if selected(7) {
        scenarios::eval::run(&mut report).await;
    }
    if selected(8) {
        scenarios::hosts::run(&mut report).await;
    }

    println!();
    println!("| Scenario | Item | Mode | Result | Detail |");
    println!("|---|---|---|---|---|");
    let mut failures = 0;
    for outcome in &report.outcomes {
        let (result, detail) = match &outcome.verdict {
            Verdict::Pass(detail) => ("PASS", detail),
            Verdict::Fail(detail) => {
                failures += 1;
                ("FAIL", detail)
            }
            Verdict::Skip(detail) => ("SKIP", detail),
        };
        println!(
            "| `{}` | {} | {} | {result} | {} |",
            outcome.id,
            outcome.item,
            outcome.mode.label(),
            common::short(detail, 160).replace('|', "/")
        );
    }
    let live = report.outcomes.iter().filter(|outcome| outcome.mode == Mode::Live).count();
    println!();
    println!("{} scenarios ({live} live), {failures} failed", report.outcomes.len());
    if failures > 0 {
        std::process::exit(1);
    }
    Ok(())
}
