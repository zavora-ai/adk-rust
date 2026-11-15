//! Measures the framework's per-turn or per-iteration cost on a growing history, with a
//! scripted model and no API key.
//!
//! ```bash
//! cargo run -p adk-bench --release --example long_history -- turns 1000
//! cargo run -p adk-bench --release --example long_history -- loop 2000 --payload 4096
//! cargo run -p adk-bench --release --example long_history -- turns 1000 --window 50 --json
//! ```

use adk_bench::{LongHistoryConfig, LongHistoryMode, run_long_history};

fn usage() -> ! {
    eprintln!(
        "usage: long_history <turns|loop> <steps> [--window <events>] [--payload <bytes>] [--json]"
    );
    std::process::exit(2)
}

fn flag(args: &[String], name: &str) -> Option<usize> {
    let index = args.iter().position(|arg| arg == name)?;
    Some(args.get(index + 1).and_then(|value| value.parse().ok()).unwrap_or_else(|| usage()))
}

#[tokio::main]
async fn main() -> adk_bench::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = match args.first().map(String::as_str) {
        Some("turns") => LongHistoryMode::Turns,
        Some("loop") => LongHistoryMode::Loop,
        _ => usage(),
    };
    let steps = args.get(1).and_then(|steps| steps.parse().ok()).unwrap_or_else(|| usage());
    let config = LongHistoryConfig {
        mode,
        steps,
        tool_payload_bytes: flag(&args, "--payload").unwrap_or(200),
        history_max_events: flag(&args, "--window"),
    };

    let report = run_long_history(&config).await?;
    if args.iter().any(|arg| arg == "--json") {
        println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
        return Ok(());
    }
    let clock = if report.step_cpu.is_empty() { "wall" } else { "CPU" };
    println!("mode: {mode:?}, steps: {steps}, session events: {}", report.session_events);
    println!("| Steps | Mean {clock} per step | Median | p95 |");
    println!("|-------|------------------|--------|-----|");
    for (label, stats) in [("first tenth", &report.first), ("last tenth", &report.last)] {
        println!(
            "| {label} | {} µs | {} µs | {} µs |",
            stats.mean_us, stats.median_us, stats.p95_us
        );
    }
    println!("growth (last / first): {:.1}x", report.growth());
    if let Some(cpu) = report.total_cpu {
        println!("process CPU: {:.3} s", cpu.as_secs_f64());
    }
    if let Some(rss) = report.peak_rss_bytes {
        println!("peak RSS: {:.1} MB", rss as f64 / 1_048_576.0);
    }
    Ok(())
}
