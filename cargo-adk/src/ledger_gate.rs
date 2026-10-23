//! `cargo adk eval --ledger`: the ledger statistics over an exported activity ledger as a CI
//! regression gate (spec 3.2 §7.10, §10 E1).
//!
//! The export is one `adk_eval::ledger::LedgerRow` per line. The gate derives the daily
//! dimensions, replays the deviation test over every day, and fails when the last day's
//! deviations are meaningful: two dimensions, or one for 10 days. The cooldown is not applied,
//! so a regression keeps failing for as long as it lasts.

use std::collections::BTreeMap;
use std::path::PathBuf;

use adk_eval::ledger::{
    self, BASELINE_WINDOW_DAYS, DailyValue, DeviationConfig, LedgerAssessment, MEANINGFUL_RUN_DAYS,
    RECENT_WINDOW_DAYS, WARM_UP_DAYS,
};
use serde::Serialize;

use crate::eval_cmd::EvalFormat;

/// Everything `cargo adk eval --ledger` was asked to do.
#[derive(Debug, Clone)]
pub(crate) struct LedgerGateOptions {
    pub ledger: PathBuf,
    pub floors: Vec<String>,
    pub format: EvalFormat,
    pub output: Option<PathBuf>,
}

/// The `--format json` report.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LedgerGateReport<'a> {
    ledger: String,
    rows: usize,
    passed: bool,
    #[serde(flatten)]
    assessment: &'a LedgerAssessment,
}

/// Runs the ledger statistics over the export at `options.ledger`.
///
/// Returns `Ok(true)` when the last day has no meaningful deviation and `Ok(false)` when it
/// has one; the report has been written either way.
///
/// # Errors
///
/// Returns an error when the export cannot be read or parsed, has no rows, no dimension has
/// finished its warm-up, a `--floor` is malformed or names a dimension the export does not
/// have, `--format junit` is asked for, or the report cannot be written.
pub(crate) fn run_ledger_gate(options: &LedgerGateOptions) -> Result<bool, String> {
    let json = match options.format {
        EvalFormat::Table => false,
        EvalFormat::Json => true,
        EvalFormat::Junit => {
            return Err("--format junit has no ledger report; use --format table or json with \
                        --ledger"
                .to_string());
        }
    };
    let path = options.ledger.display();
    let text = std::fs::read_to_string(&options.ledger)
        .map_err(|e| format!("failed to read the ledger export {path}: {e}"))?;
    let rows = ledger::parse_jsonl(&text).map_err(|e| format!("{path}: {e}"))?;
    let series = ledger::daily_series(&rows);
    let config = floor_config(&options.floors, &series)?;
    let Some(assessment) = ledger::assess(&series, &config) else {
        return Err(format!(
            "{path} has no ledger rows, so there is nothing to check; export a ledger that \
             covers at least {} days",
            WARM_UP_DAYS + max_window()
        ));
    };

    let most_learned = assessment
        .report
        .dimensions
        .iter()
        .map(|dimension| dimension.warm_up.observed_days)
        .max()
        .unwrap_or(0);
    if most_learned < WARM_UP_DAYS {
        return Err(format!(
            "{path}: no dimension has finished its warm-up of {WARM_UP_DAYS} baseline days (the \
             most is {most_learned}), so the gate would pass unchecked; the baseline is the \
             {BASELINE_WINDOW_DAYS} days before the last {} days, so export a ledger that covers \
             at least {} days",
            max_window(),
            WARM_UP_DAYS + max_window()
        ));
    }

    let passed = !assessment.meaningful;
    let rendered = if json {
        let report = LedgerGateReport {
            ledger: path.to_string(),
            rows: rows.len(),
            passed,
            assessment: &assessment,
        };
        serde_json::to_string_pretty(&report)
            .map_err(|e| format!("failed to serialize the report: {e}"))?
    } else {
        table(&path.to_string(), rows.len(), &assessment, config.k)
    };
    match &options.output {
        Some(output) => {
            std::fs::write(output, rendered)
                .map_err(|e| format!("failed to write {}: {e}", output.display()))?;
            eprintln!("report written to {}", output.display());
        }
        None => println!("{rendered}"),
    }
    Ok(passed)
}

/// The longest comparison window, in days.
fn max_window() -> u32 {
    RECENT_WINDOW_DAYS.iter().copied().max().unwrap_or(0)
}

/// The default deviation settings with each `--floor DIMENSION=VALUE` applied.
fn floor_config(
    floors: &[String],
    series: &BTreeMap<String, Vec<DailyValue>>,
) -> Result<DeviationConfig, String> {
    let mut config = DeviationConfig::default();
    for floor in floors {
        let Some((dimension, value)) = floor.split_once('=') else {
            return Err(format!(
                "--floor {floor:?} is not DIMENSION=VALUE, such as --floor tool_call.count=2"
            ));
        };
        let dimension = dimension.trim();
        let value = value
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .ok_or_else(|| {
                format!("--floor {floor:?}: the floor must be a finite number of at least 0")
            })?;
        if !series.contains_key(dimension) {
            let known: Vec<&str> = series.keys().map(String::as_str).collect();
            return Err(format!(
                "--floor names {dimension}, which the ledger does not have, so it would change \
                 nothing; its dimensions are {}",
                known.join(", ")
            ));
        }
        config = config.with_floor(dimension, value);
    }
    Ok(config)
}

fn table(path: &str, rows: usize, assessment: &LedgerAssessment, k: f64) -> String {
    use std::fmt::Write;

    let report = &assessment.report;
    let days = (assessment.last_day - assessment.first_day).num_days() + 1;
    let window_end = ledger::baseline_window_end(report.as_of);
    let windows: Vec<String> = RECENT_WINDOW_DAYS.iter().map(u32::to_string).collect();
    let mut out = String::new();
    let _ = writeln!(out, "Ledger: {path}");
    let _ = writeln!(
        out,
        "{rows} rows over {days} days, {} to {}; tested as of {}",
        assessment.first_day, assessment.last_day, report.as_of
    );
    let _ = writeln!(
        out,
        "Baseline: the {BASELINE_WINDOW_DAYS} days to {window_end}, compared with the last {} \
         days; deviation when |recent - median| > max({k}·MAD, floor)",
        windows.join(" and ")
    );

    let width = report
        .dimensions
        .iter()
        .map(|dimension| dimension.dimension.len())
        .max()
        .unwrap_or(0)
        .max("Dimension".len());
    let _ = writeln!(
        out,
        "\n{:<width$}  {:<7}  {:>9}  {:>9}  {:>9}  {:>9}  {:>9}  Result",
        "Dimension", "Class", "Median", "MAD", "Threshold", "Last 7", "Last 14"
    );
    let number = |value: Option<f64>| value.map_or_else(|| "-".to_string(), |v| format!("{v:.1}"));
    for dimension in &report.dimensions {
        let run = assessment.runs.iter().find(|run| run.dimension == dimension.dimension);
        for class in &dimension.classes {
            let recent = |days: u32| {
                number(
                    class
                        .windows
                        .iter()
                        .find(|window| window.days == days)
                        .and_then(|window| window.recent_median),
                )
            };
            let result = if !dimension.warm_up.is_complete() {
                format!(
                    "learning ({} of {} days)",
                    dimension.warm_up.observed_days, dimension.warm_up.required_days
                )
            } else if class.baseline.is_none() {
                "no values".to_string()
            } else if class.deviating {
                match run {
                    Some(run) => format!("DEVIATES ({} days)", run.days),
                    None => "DEVIATES".to_string(),
                }
            } else {
                "ok".to_string()
            };
            let class_name = match class.class {
                ledger::DayClass::Weekday => "weekday",
                ledger::DayClass::Weekend => "weekend",
            };
            let _ = writeln!(
                out,
                "{:<width$}  {class_name:<7}  {:>9}  {:>9}  {:>9}  {:>9}  {:>9}  {result}",
                dimension.dimension,
                number(class.baseline.map(|baseline| baseline.median)),
                number(class.baseline.map(|baseline| baseline.mad)),
                number(class.threshold),
                recent(7),
                recent(14),
            );
        }
    }

    let runs: Vec<String> = assessment
        .runs
        .iter()
        .map(|run| format!("{} for {} days", run.dimension, run.days))
        .collect();
    let _ = writeln!(out);
    if assessment.meaningful {
        let _ = write!(out, "FAIL: meaningful deviation: {}", runs.join(", "));
    } else if runs.is_empty() {
        let _ = write!(out, "PASS: no dimension deviates");
    } else {
        let _ = write!(
            out,
            "PASS: {} deviates, which is not meaningful until it lasts {MEANINGFUL_RUN_DAYS} days \
             or a second dimension deviates",
            runs.join(", ")
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series() -> BTreeMap<String, Vec<DailyValue>> {
        BTreeMap::from([("tool_call.count".to_string(), Vec::new())])
    }

    #[test]
    fn floors_parse_and_apply() {
        let config = floor_config(&["tool_call.count = 2.5".to_string()], &series()).unwrap();
        assert_eq!(config.floor("tool_call.count"), 2.5);
    }

    #[test]
    fn malformed_and_unknown_floors_are_refused() {
        let error = |floor: &str| floor_config(&[floor.to_string()], &series()).unwrap_err();
        assert!(error("tool_call.count").contains("DIMENSION=VALUE"));
        assert!(error("tool_call.count=-1").contains("at least 0"));
        assert!(error("tool_call.count=NaN").contains("finite"));
        let unknown = error("tool_calls.count=1");
        assert!(unknown.contains("does not have"), "{unknown}");
        assert!(unknown.contains("tool_call.count"), "{unknown}");
    }
}
