//! `cargo adk eval --ledger` runs the ledger statistics over an exported activity ledger and
//! exits non-zero on a meaningful deviation (spec 3.2 §7.10, §10 E1).
//!
//! The fixtures are six weeks of a content-free ledger from 2026-08-31 to 2026-10-11. In
//! `drift.jsonl` the last two weeks' weekdays finish at 19:20 instead of 17:30 and make eight
//! tool calls instead of four; `steady.jsonl` keeps the first four weeks' routine throughout.

use std::path::{Path, PathBuf};
use std::process::Output;

use tempfile::TempDir;

/// The floor a CI gate sets for the ledger's noisy tool latency, in milliseconds.
const LATENCY_FLOOR: [&str; 2] = ["--floor", "tool_call.duration_ms=50"];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ledger").join(name)
}

fn cargo_adk_eval(args: &[&str]) -> Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_cargo-adk"))
        .args(["adk", "eval"])
        .args(args)
        .output()
        .expect("cargo-adk runs")
}

fn text(output: &Output) -> String {
    format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn gate(ledger: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["--ledger", ledger.to_str().unwrap()];
    args.extend(LATENCY_FLOOR);
    args.extend(extra);
    cargo_adk_eval(&args)
}

#[test]
fn a_steady_ledger_passes() {
    let output = gate(&fixture("steady.jsonl"), &[]);

    assert!(output.status.success(), "{}", text(&output));
    let report = text(&output);
    assert!(report.contains("288 rows over 42 days, 2026-08-31 to 2026-10-11"), "{report}");
    assert!(report.contains("PASS: no dimension deviates"), "{report}");
    assert!(!report.contains("DEVIATES"), "{report}");
}

#[test]
fn a_drifting_ledger_fails() {
    let output = gate(&fixture("drift.jsonl"), &[]);

    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let report = text(&output);
    assert!(
        report.contains(
            "FAIL: meaningful deviation: activity.last_minute for 10 days, tool_call.count for \
             10 days"
        ),
        "{report}"
    );
    let deviating: Vec<&str> = report.lines().filter(|line| line.contains("DEVIATES")).collect();
    assert_eq!(deviating.len(), 2, "{report}");
    assert!(deviating.iter().all(|line| line.contains("weekday")), "{report}");
}

#[test]
fn json_output_carries_the_assessment() {
    let dir = TempDir::new().unwrap();
    let out = dir.path().join("ledger.json");
    let output =
        gate(&fixture("drift.jsonl"), &["--format", "json", "--output", out.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(report["passed"], false);
    assert_eq!(report["meaningful"], true);
    assert_eq!(report["rows"], 328);
    assert_eq!(report["lastDay"], "2026-10-11");
    let runs: Vec<(&str, u64)> = report["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| (run["dimension"].as_str().unwrap(), run["days"].as_u64().unwrap()))
        .collect();
    assert_eq!(runs, [("activity.last_minute", 10), ("tool_call.count", 10)]);
}

#[test]
fn one_dimension_is_meaningful_only_after_ten_days() {
    // A floor of 5 tool calls leaves the finish time as the only deviating dimension.
    let floors = ["--floor", "tool_call.count=5"];

    let output = gate(&fixture("drift.jsonl"), &floors);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(
        text(&output).contains("FAIL: meaningful deviation: activity.last_minute for 10 days"),
        "{}",
        text(&output)
    );

    // Without the export's last day the run is nine days long, which is not meaningful.
    let dir = TempDir::new().unwrap();
    let shorter = dir.path().join("drift-to-2026-10-10.jsonl");
    let rows: String = std::fs::read_to_string(fixture("drift.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| !line.contains(r#""ts":"2026-10-11"#))
        .map(|line| format!("{line}\n"))
        .collect();
    std::fs::write(&shorter, rows).unwrap();

    let output = gate(&shorter, &floors);
    assert!(output.status.success(), "{}", text(&output));
    assert!(
        text(&output).contains("PASS: activity.last_minute for 9 days deviates"),
        "{}",
        text(&output)
    );
}

#[test]
fn a_ledger_too_short_to_learn_from_is_refused() {
    let dir = TempDir::new().unwrap();
    let short = dir.path().join("short.jsonl");
    let rows: String = std::fs::read_to_string(fixture("steady.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| line.contains(r#""ts":"2026-09-0"#))
        .map(|line| format!("{line}\n"))
        .collect();
    std::fs::write(&short, rows).unwrap();

    let output = cargo_adk_eval(&["--ledger", short.to_str().unwrap()]);

    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    let report = text(&output);
    assert!(report.contains("no dimension has finished its warm-up"), "{report}");
    assert!(report.contains("at least 28 days"), "{report}");
}

#[test]
fn bad_input_fails_with_guidance() {
    let dir = TempDir::new().unwrap();
    let empty = dir.path().join("empty.jsonl");
    std::fs::write(&empty, "\n").unwrap();
    let output = cargo_adk_eval(&["--ledger", empty.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("has no ledger rows"), "{}", text(&output));

    let broken = dir.path().join("broken.jsonl");
    std::fs::write(&broken, "{\"ts\":\"yesterday\"}\n").unwrap();
    let output = cargo_adk_eval(&["--ledger", broken.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("ledger line 1 is not a ledger row"), "{}", text(&output));

    let steady = fixture("steady.jsonl");
    let output =
        cargo_adk_eval(&["--ledger", steady.to_str().unwrap(), "--floor", "tool_calls.count=1"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("which the ledger does not have"), "{}", text(&output));

    let output = cargo_adk_eval(&["--ledger", steady.to_str().unwrap(), "--format", "junit"]);
    assert_eq!(output.status.code(), Some(1), "{}", text(&output));
    assert!(text(&output).contains("--format junit has no ledger report"), "{}", text(&output));
}

#[test]
fn ledger_mode_refuses_eval_set_flags() {
    let steady = fixture("steady.jsonl");
    let output = cargo_adk_eval(&["--ledger", steady.to_str().unwrap(), "--agent-cmd", "true"]);

    assert_eq!(output.status.code(), Some(2), "{}", text(&output));
    assert!(text(&output).contains("cannot be used with"), "{}", text(&output));
}
