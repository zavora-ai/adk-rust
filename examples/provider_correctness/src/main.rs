//! Provider request-correctness checks.
//!
//! Each scenario drives `LlmAgent` + `Runner` with a real provider client, against the live
//! API or a scripted local upstream, and asserts what the client sends and what the run does.
//!
//! ```bash
//! cargo run                       # every scenario
//! cargo run -- ML5 ML13           # only these findings (prefix match on scenario ids works too)
//! cargo run --no-default-features # baseline build for a revision without the new options
//! ```

mod common;
mod scenarios;
mod upstream;

use common::{Keys, Outcome, Verdict};

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

    let filters: Vec<String> = std::env::args().skip(1).map(|arg| arg.to_lowercase()).collect();
    let selected = |finding: &str| {
        filters.is_empty() || filters.iter().any(|filter| filter == &finding.to_lowercase())
    };
    let keys = Keys::from_env();
    let mut outcomes: Vec<Outcome> = Vec::new();

    println!(
        "provider_correctness ({} build)",
        if cfg!(feature = "fixed") { "fixed" } else { "baseline" }
    );
    if selected("ML5") {
        scenarios::ml5::run(&keys, &mut outcomes).await;
    }
    if selected("ML6") {
        scenarios::ml6::run(&keys, &mut outcomes).await;
    }
    if selected("ML12") {
        scenarios::ml12::run(&keys, &mut outcomes).await;
    }
    if selected("ML13") {
        scenarios::ml13::run(&keys, &mut outcomes).await;
    }
    if selected("ML14") {
        scenarios::ml14::run(&keys, &mut outcomes).await;
    }
    if selected("ML15") {
        scenarios::ml15::run(&keys, &mut outcomes).await;
    }
    if selected("ML18") {
        scenarios::ml18::run(&keys, &mut outcomes).await;
    }
    if selected("ML25") {
        scenarios::ml25::run(&keys, &mut outcomes).await;
    }
    if selected("ML29") {
        scenarios::ml29::run(&keys, &mut outcomes).await;
    }

    println!();
    println!("| Scenario | Finding | Mode | Result | Detail |");
    println!("|---|---|---|---|---|");
    let mut failures = 0;
    for outcome in &outcomes {
        let (result, detail) = match &outcome.verdict {
            Verdict::Pass(detail) => ("PASS", detail),
            Verdict::Fail(detail) => {
                failures += 1;
                ("FAIL", detail)
            }
            Verdict::Skip(detail) => ("SKIP", detail),
        };
        println!(
            "| {} | {} | {} | {result} | {} |",
            outcome.id,
            outcome.finding,
            outcome.mode.label(),
            detail.replace('|', "/")
        );
    }
    println!();
    println!("{} scenarios, {failures} failed", outcomes.len());
    if failures > 0 {
        std::process::exit(1);
    }
    Ok(())
}
