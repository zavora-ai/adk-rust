//! Scenarios S1–S19 of spec 3.5 (§5, §11): product-level runs of `adk-website` against local
//! fixtures, asserting each pass condition on stores, audit records, releases and the hosts'
//! contents.
//!
//! ```text
//! cargo run --manifest-path examples/website/Cargo.toml                 # every scenario, scripted
//! cargo run --manifest-path examples/website/Cargo.toml -- S1 S5        # some scenarios
//! cargo run --manifest-path examples/website/Cargo.toml -- --live       # agent turns on claude-sonnet-5-5
//! cargo run --manifest-path examples/website/Cargo.toml -- --live --provider gemini
//! ```

mod checks;
mod fixture;
mod live;
mod recording;
mod scenarios;

use anyhow::Result;

use crate::checks::Checks;
use crate::live::Mode;

fn verdict(result: &Result<Checks>, known: &[&str]) -> String {
    match result {
        Ok(checks) if checks.passed() => format!("PASS ({})", checks.tally()),
        Ok(checks) if checks.failures().iter().all(|f| known.contains(&f.as_str())) => {
            format!(
                "GAP ({}; waiting on the runtime: {})",
                checks.tally(),
                checks.failures().join("; ")
            )
        }
        Ok(checks) => format!("FAIL ({}: {})", checks.tally(), checks.failures().join("; ")),
        Err(error) => format!("ERROR ({error:#})"),
    }
}

/// The S19 soak (spec 3.5 §11 "Soak"): `soak [--scaled] [--report <path>]`, 24 simulated
/// hours of 50,000 products by default; writes the report as JSON and prints it.
async fn soak(args: &[String]) -> Result<()> {
    use scenarios::s19_constant::{Scale, soak};
    let scale = if args.iter().any(|a| a == "--scaled") { Scale::SCALED } else { Scale::FULL_DAY };
    let report_path =
        args.iter().position(|a| a == "--report").and_then(|i| args.get(i + 1)).cloned();
    println!("S19 soak: {} products over {} simulated hours\n", scale.products, scale.hours);
    let (checks, report) = soak(scale).await?;
    let text = serde_json::to_string_pretty(&report)?;
    println!("\n{text}");
    if let Some(path) = report_path {
        std::fs::write(&path, &text)?;
        println!("report written to {path}");
    }
    println!("\n{}", if checks.passed() { "PASS" } else { "FAIL" });
    if !checks.passed() {
        std::process::exit(1);
    }
    Ok(())
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    adk_core::ensure_crypto_provider();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("record") {
        return recording::record().await;
    }
    if args.first().map(String::as_str) == Some("soak") {
        return soak(&args).await;
    }
    if args.first().map(String::as_str) == Some("wordpress") {
        // The nightly tier, against the compose fixture (fixtures/wordpress/compose.yaml).
        let origin = args.get(1).cloned().unwrap_or_else(|| "http://127.0.0.1:8080".into());
        let infected = args.iter().any(|a| a == "--infected");
        println!(
            "WordPress fixture at {origin}{}\n",
            if infected { " (infected overlay)" } else { "" }
        );
        let checks = recording::against_live(url::Url::parse(&origin)?, infected).await?;
        println!("\n{}", if checks.passed() { "PASS" } else { "FAIL" });
        if !checks.passed() {
            std::process::exit(1);
        }
        return Ok(());
    }
    let value =
        |flag: &str| args.iter().position(|arg| arg == flag).and_then(|i| args.get(i + 1)).cloned();
    let live = args.iter().any(|arg| arg == "--live");
    let provider = value("--provider").unwrap_or_else(|| "anthropic".into());
    let report = value("--report").unwrap_or_default();
    let selected: Vec<String> = args
        .iter()
        .filter(|arg| !arg.starts_with("--") && **arg != provider && **arg != report)
        .map(|arg| arg.to_uppercase())
        .collect();

    let mode = if live {
        live::load_env();
        live::model_for(&provider)?
    } else {
        Mode::Scripted
    };
    println!("adk-website scenarios (spec 3.5 §5, §11), model: {}\n", mode.name());
    let report_path = value("--report");
    let mut rows = Vec::new();
    let mut gaps = Vec::new();
    let mut details = String::new();
    let mut failed = false;
    for scenario in scenarios::all() {
        if !selected.is_empty() && !selected.iter().any(|s| s == scenario.id) {
            continue;
        }
        if live && !scenario.agent {
            rows.push(format!(
                "| {} | {} | — | no agent turn: the scripted run is the run |",
                scenario.id, scenario.title
            ));
            continue;
        }
        println!("  {} {}", scenario.id, scenario.title);
        let result = (scenario.run)(mode.clone()).await;
        let shown = verdict(&result, scenario.known);
        if let Ok(checks) = &result {
            for known in scenario.known {
                if checks.all().iter().any(|c| c.name == *known && c.passed) {
                    println!(
                        "    note: the known gap \"{known}\" now passes; remove it from the list"
                    );
                }
            }
        }
        if let Ok(checks) = &result {
            gaps.extend(checks.gaps().iter().map(|gap| format!("- {}: {gap}", scenario.id)));
            details.push_str(&format!("\n### {} — {}\n\n", scenario.id, scenario.title));
            for check in checks.all() {
                details.push_str(&format!(
                    "- {} {}: {}\n",
                    if check.passed { "pass" } else { "FAIL" },
                    check.name,
                    check.evidence
                ));
            }
        }
        if let Err(error) = &result {
            println!("    ERROR {error:#}");
        }
        failed |= !(shown.starts_with("PASS") || shown.starts_with("GAP"));
        rows.push(format!("| {} | {} | {} | {shown} |", scenario.id, scenario.title, mode.name()));
    }
    println!("\n| Scenario | Capability | Model | Result |\n|---|---|---|---|");
    for row in &rows {
        println!("{row}");
    }
    if !gaps.is_empty() {
        println!("\nRuntime gaps:");
        for gap in &gaps {
            println!("{gap}");
        }
    }
    if let Some(path) = report_path {
        let table: Vec<&String> = rows.iter().collect();
        let text = format!(
            "# Scenarios, model {}\n\n| Scenario | Capability | Model | Result |\n|---|---|---|---|\n{}\n\n## Checks\n{details}\n## Runtime gaps\n\n{}\n",
            mode.name(),
            table.iter().map(|r| r.as_str()).collect::<Vec<_>>().join("\n"),
            gaps.join("\n")
        );
        std::fs::write(&path, text)?;
        println!("\nreport written to {path}");
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}
