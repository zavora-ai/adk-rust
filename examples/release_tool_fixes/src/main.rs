//! Checks for the second set of 3.0 docs-audit fixes.
//!
//! Each scenario drives the real stack — `LlmAgent` + `Runner` with a provider client,
//! `RagTool`, `McpToolset` over a stdio server, `AgentTool`, `ActionNodeExecutor`,
//! `ProcessBackend` under Seatbelt, `McpHttpClientBuilder`, or `AcpSession` against an
//! `AcpServer` — against a live API, a scripted local upstream, or in process, and asserts
//! the documented behaviour.
//!
//! ```bash
//! cargo run            # every scenario
//! cargo run -- 1 4     # only these audit items
//! ```

mod common;
mod scenarios;
mod upstream;

use common::{Mode, Report, Verdict};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Child-process roles speak their protocol on stdout, so they start before any logging.
    match args.first().map(String::as_str) {
        Some(scenarios::mcp::SERVER_FLAG) => return scenarios::mcp::serve_stdio().await,
        Some(scenarios::launcher::LAUNCHER_FLAG) => {
            // The parent loaded the API keys into the environment this process inherits.
            let target = args.get(1).cloned().unwrap_or_default();
            return scenarios::launcher::serve(&target).await;
        }
        Some(scenarios::acp::SERVER_FLAG) => {
            let timeout_ms = args.get(1).and_then(|value| value.parse().ok()).unwrap_or(5_000);
            return scenarios::acp::serve_stdio(timeout_ms).await;
        }
        _ => {}
    }

    // The shell may export stale keys, so the file's values win.
    let env_file = std::env::var("ADK_ENV_FILE")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env").to_string());
    if dotenvy::from_path_override(&env_file).is_err() {
        let _ = dotenvy::dotenv_override();
    }
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "error".into()),
        )
        .init();

    let items: Vec<u8> = args.iter().filter_map(|arg| arg.parse().ok()).collect();
    let selected = |item: u8| items.is_empty() || items.contains(&item);
    let gemini_key = ["GOOGLE_API_KEY", "GEMINI_API_KEY"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.trim().is_empty()));

    let mut report = Report::default();
    if selected(1) {
        scenarios::rag::run(gemini_key.as_deref(), &mut report).await;
    }
    if selected(2) || selected(7) {
        scenarios::mcp::run(gemini_key.as_deref(), &mut report).await;
    }
    if selected(3) {
        scenarios::agent_tool::run(&mut report).await;
    }
    if selected(4) {
        scenarios::action::run(&mut report).await;
    }
    if selected(5) {
        scenarios::sandbox::run(&mut report).await;
    }
    if selected(6) {
        scenarios::progress::run(&mut report).await;
    }
    if selected(7) {
        scenarios::acp::run(&mut report).await;
    }
    if selected(10) {
        scenarios::agent_tool::run_output_key(gemini_key.as_deref(), &mut report).await;
    }
    if selected(11) {
        scenarios::launcher::run(gemini_key.as_deref(), &mut report).await;
    }

    println!();
    println!("| Scenario | Item | Mode | Result |");
    println!("|---|---|---|---|");
    let mut failures = 0;
    for outcome in &report.outcomes {
        let result = match &outcome.verdict {
            Verdict::Pass(_) => "PASS",
            Verdict::Fail(_) => {
                failures += 1;
                "FAIL"
            }
            Verdict::Skip(_) => "SKIP",
        };
        println!("| `{}` | {} | {} | {result} |", outcome.id, outcome.item, outcome.mode.label());
    }
    let live = report.outcomes.iter().filter(|outcome| outcome.mode == Mode::Live).count();
    println!();
    println!("{} scenarios ({live} live), {failures} failed", report.outcomes.len());
    if failures > 0 {
        std::process::exit(1);
    }
    Ok(())
}
