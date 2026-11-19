//! `cargo adk eval`: runs eval sets against an agent binary over a JSONL protocol.
//!
//! The CLI cannot construct a Rust agent, so the agent under test runs as its own
//! process. `--agent-cmd` starts it once per eval run; the CLI then writes one request
//! line per turn to its stdin and reads one response line from its stdout:
//!
//! ```text
//! → {"case_id":"current_weather","turn":0,"user_text":"Weather in Nairobi?","session_id":"…"}
//! ← {"text":"Sunny, 24°C.","tool_calls":[{"name":"get_weather","args":{"city":"Nairobi"}}]}
//! ← {"error":"model quota exhausted"}
//! ```
//!
//! Every turn of a case shares one `session_id`, and each case gets a new one, so the
//! agent keys its conversation state by `session_id`. The responses are scored with
//! `adk_eval::Evaluator`, so the criteria behave exactly as they do in a Rust test.
//!
//! A turn that shows images adds `images`: each fixture's absolute path, its SHA-256 (already
//! verified) and its MIME type. A tool call may carry the tool's `response`, which is how
//! vision answers reach the vision criteria:
//!
//! ```text
//! → {"case_id":"shapes","turn":0,"user_text":"…","session_id":"…",
//!    "images":[{"path":"/…/shapes.png","digest":"sha256:…","mime_type":"image/png"}]}
//! ← {"tool_calls":[{"name":"vision_detect","args":{},"response":{"regions":[{"box":[80,60,320,220],"label":"red rectangle"}]}}]}
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use adk_core::{
    AdkError, Agent, Content, Event, EventStream, InvocationContext, Llm, Part, RunBudget,
    RunConfig,
};
use adk_eval::{
    BaselineStore, EvalCase, EvalError, EvaluationConfig, EvaluationCriteria, EvaluationReport,
    EvaluationResult, Evaluator, Failure, ImageFixture, JunitReporter, MultiRunConfig,
    MultiRunReport, TestFile,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// Output formats for `cargo adk eval`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum EvalFormat {
    /// Human-readable summary
    Table,
    /// Every report as pretty-printed JSON
    Json,
    /// One JUnit XML test suite covering every case
    Junit,
}

/// Everything `cargo adk eval` was asked to do.
#[derive(Debug, Clone)]
pub(crate) struct EvalOptions {
    pub path: PathBuf,
    pub agent_cmd: String,
    pub criteria: Option<PathBuf>,
    pub judge_model: Option<String>,
    pub save_baseline: bool,
    pub check_regression: bool,
    pub baseline: Option<PathBuf>,
    pub tolerance: f64,
    pub format: EvalFormat,
    pub output: Option<PathBuf>,
    pub turn_timeout: Duration,
    pub num_runs: usize,
    pub min_pass_rate: f64,
    pub judge_max_calls: Option<u64>,
    pub judge_max_cost_usd: Option<f64>,
}

/// Runs the eval sets at `options.path` `options.num_runs` times against the agent
/// `options.agent_cmd` starts.
///
/// Returns `Ok(true)` when every case reached the minimum pass rate (every run, by
/// default), no regression was found and every run completed, and `Ok(false)` otherwise;
/// the report has been written either way.
///
/// # Errors
///
/// Returns an error, before running any case, when the eval set, criteria, judge model or
/// run settings cannot be loaded, an eval set has no cases, or `--check-regression` has no
/// baseline to compare with; and
/// after the run when the baseline cannot be read or saved, or the report cannot be
/// written.
pub(crate) async fn run_eval(options: &EvalOptions) -> Result<bool, String> {
    let multi_run = MultiRunConfig::new(options.num_runs)
        .and_then(|config| config.with_min_pass_rate(options.min_pass_rate))
        .map_err(|e| e.to_string())?;
    let files = discover_test_files(&options.path)?;
    let mut test_files = Vec::with_capacity(files.len());
    for file in &files {
        let test_file =
            TestFile::load(file).map_err(|e| format!("failed to load {}: {e}", file.display()))?;
        if test_file.eval_cases.is_empty() {
            return Err(format!(
                "{} has no eval cases, so it would pass unchecked; add at least one case",
                file.display()
            ));
        }
        test_files.push(test_file);
    }

    let criteria = load_criteria(options.criteria.as_deref())?;
    let config = EvaluationConfig {
        continue_on_failure: true,
        collect_turn_details: true,
        ..EvaluationConfig::with_criteria(criteria)
    };
    let mut evaluator = match options.judge_model.as_deref() {
        Some(model) => Evaluator::with_llm_judge(config, judge_from_env(model)?),
        None => {
            let needs_judge = judged_criteria(&config.criteria);
            if !needs_judge.is_empty() {
                return Err(format!(
                    "the criteria {} need an LLM judge; pass --judge-model with a gemini-*, \
                     claude-*, or gpt-* model id and set its API key",
                    needs_judge.join(", ")
                ));
            }
            Evaluator::new(config)
        }
    };
    // Judge calls count against one budget across every run; an unlimited budget still
    // counts them for the report.
    let judge_budget = judge_budget(options)?;
    if options.judge_model.is_some() {
        evaluator.set_run_config(RunConfig::builder().budget(judge_budget).build());
    }

    // Checked before any case runs, so a missing baseline does not cost a full eval run.
    let baseline_path = options.baseline.clone().unwrap_or_else(|| default_baseline(&options.path));
    if options.check_regression && !baseline_path.exists() {
        return Err(format!(
            "--check-regression needs a baseline, and there is none at {}; save one first with \
             --save-baseline",
            baseline_path.display()
        ));
    }

    let command = Arc::new(AgentCommand::new(&options.agent_cmd, options.turn_timeout));
    let mut runs: Vec<Vec<EvaluationReport>> = vec![Vec::new(); test_files.len()];
    let mut stopped = None;
    'runs: for run in 0..multi_run.num_runs {
        let mut this_run = Vec::with_capacity(test_files.len());
        for test_file in &test_files {
            match evaluate_file(&evaluator, &command, test_file).await {
                Ok(report) => this_run.push(report),
                Err(refusal) => {
                    stopped = Some(format!("run {} of {}: {refusal}", run + 1, multi_run.num_runs));
                    break 'runs;
                }
            }
        }
        for (file_runs, report) in runs.iter_mut().zip(this_run) {
            file_runs.push(report);
        }
    }
    command.shutdown().await;

    let reports: Vec<MultiRunReport> = test_files
        .iter()
        .zip(runs)
        .map(|(test_file, file_runs)| {
            let report = MultiRunReport::from_runs(
                &test_file.eval_set_id,
                multi_run.num_runs,
                multi_run.min_pass_rate,
                file_runs,
            );
            match &stopped {
                Some(reason) => report.with_stopped(reason.clone()),
                None => report,
            }
        })
        .collect();
    let mut metrics: HashMap<String, HashMap<String, f64>> = HashMap::new();
    for report in &reports {
        for (metric, cases) in report.baseline_metrics() {
            metrics.entry(metric).or_default().extend(cases);
        }
    }
    write_report(options, &reports)?;
    if let Some(usage) = evaluator.run_config().and_then(|config| config.budget_tracker.as_ref()) {
        let usage = usage.usage();
        eprintln!(
            "judge: {} model call(s), {} tokens, ${:.4}",
            usage.model_calls,
            usage.total_tokens,
            usage.cost_micro_usd as f64 / 1_000_000.0
        );
    }
    if let Some(reason) = &stopped {
        eprintln!(
            "stopped: {reason}; raise --judge-max-calls or --judge-max-cost-usd to finish every run"
        );
    }

    let regressions = if options.check_regression {
        BaselineStore::new(&baseline_path)
            .check_regressions(&metrics, options.tolerance)
            .map_err(|e| format!("regression check failed: {e}"))?
    } else {
        Vec::new()
    };

    for regression in &regressions {
        let current = regression
            .current_value
            .map_or_else(|| "missing".to_string(), |value| format!("{value:.3}"));
        eprintln!(
            "regression: {} [{}]: baseline {:.3}, current {current}, tolerance {:.3}",
            regression.metric_name,
            regression.case_id,
            regression.baseline_value,
            options.tolerance
        );
    }

    if options.save_baseline {
        if stopped.is_some() {
            return Err(format!(
                "not saving a baseline: only {} of {} runs completed",
                reports.first().map_or(0, MultiRunReport::completed_runs),
                multi_run.num_runs
            ));
        }
        let mut incomplete: Vec<&str> = reports
            .iter()
            .flat_map(|report| report.runs.iter().flat_map(|run| &run.results))
            .filter(|r| r.failures.iter().any(|f| f.criterion == "execution"))
            .map(|r| r.eval_id.as_str())
            .collect();
        incomplete.sort_unstable();
        incomplete.dedup();
        if !incomplete.is_empty() {
            return Err(format!(
                "not saving a baseline: case(s) {} did not run to completion, so the baseline \
                 would not cover them",
                incomplete.join(", ")
            ));
        }
        let eval_set_id =
            test_files.iter().map(|file| file.eval_set_id.as_str()).collect::<Vec<_>>().join("+");
        BaselineStore::new(&baseline_path)
            .save(&eval_set_id, &metrics)
            .map_err(|e| format!("failed to save baseline: {e}"))?;
        eprintln!("baseline saved to {}", baseline_path.display());
    }

    Ok(reports.iter().all(MultiRunReport::all_passed) && regressions.is_empty())
}

/// The budget judge calls count against across every run.
fn judge_budget(options: &EvalOptions) -> Result<RunBudget, String> {
    let mut budget = RunBudget::new();
    if let Some(calls) = options.judge_max_calls {
        if calls == 0 {
            return Err("--judge-max-calls must be at least 1".to_string());
        }
        budget = budget.max_model_calls(calls);
    }
    if let Some(usd) = options.judge_max_cost_usd {
        if !(usd.is_finite() && usd > 0.0) {
            return Err(format!("--judge-max-cost-usd must be a positive amount, got {usd}"));
        }
        budget = budget.max_cost_usd(usd);
    }
    if !budget.is_unlimited() && options.judge_model.is_none() {
        return Err(
            "--judge-max-calls and --judge-max-cost-usd limit --judge-model's calls; pass \
                    --judge-model too"
                .to_string(),
        );
    }
    Ok(budget)
}

/// Runs every case of `test_file`, each as its own conversation with the agent.
///
/// # Errors
///
/// Returns the refusal when the judge budget stops the evaluation.
async fn evaluate_file(
    evaluator: &Evaluator,
    command: &Arc<AgentCommand>,
    test_file: &TestFile,
) -> Result<EvaluationReport, EvalError> {
    let started_at = chrono::Utc::now();
    let mut results = Vec::with_capacity(test_file.eval_cases.len());
    for eval_case in &test_file.eval_cases {
        let agent: Arc<dyn Agent> = Arc::new(CommandAgent::new(Arc::clone(command), eval_case));
        let result = match evaluator.evaluate_case(agent, eval_case).await {
            Ok(result) => result,
            Err(e) if e.is_refusal() => return Err(e),
            Err(e) => EvaluationResult::failed(
                &eval_case.eval_id,
                HashMap::new(),
                vec![
                    Failure::new(
                        "execution",
                        serde_json::Value::Null,
                        serde_json::Value::String(e.to_string()),
                        0.0,
                        1.0,
                    )
                    .with_details(&e.to_string()),
                ],
                Duration::ZERO,
            ),
        };
        results.push(result);
    }
    let run_id = format!("{}_{}", test_file.eval_set_id, started_at.format("%Y%m%dT%H%M%S%.6fZ"));
    Ok(EvaluationReport::new(&run_id, results, started_at))
}

/// The `.test.json` files at `path`: the file itself, or a directory's files sorted by name.
fn discover_test_files(path: &Path) -> Result<Vec<PathBuf>, String> {
    if !path.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let entries = std::fs::read_dir(path)
        .map_err(|e| format!("failed to read directory {}: {e}", path.display()))?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|file| {
            file.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".test.json"))
        })
        .collect();
    if files.is_empty() {
        return Err(format!("no .test.json files found in {}", path.display()));
    }
    files.sort();
    Ok(files)
}

/// Loads the criteria file, or the default: an exact tool trajectory and a response
/// similarity of at least 0.8.
fn load_criteria(path: Option<&Path>) -> Result<EvaluationCriteria, String> {
    let criteria = match path {
        Some(path) => {
            let contents = std::fs::read_to_string(path)
                .map_err(|e| format!("failed to read criteria file {}: {e}", path.display()))?;
            serde_json::from_str::<EvaluationCriteria>(&contents).map_err(|e| {
                format!(
                    "criteria file {} is not valid EvaluationCriteria JSON: {e}",
                    path.display()
                )
            })?
        }
        None => {
            EvaluationCriteria::default().with_tool_trajectory(1.0).with_response_similarity(0.8)
        }
    };
    if !criteria.custom.is_empty() {
        return Err("custom criteria are not evaluated by adk_eval::Evaluator, so cargo adk eval \
                    cannot check them; remove \"custom\" from the criteria file"
            .to_string());
    }
    if !criteria.has_criteria() {
        return Err("the criteria set no thresholds, so every case would pass unchecked; set at \
                    least one, such as \"response_similarity\": 0.8"
            .to_string());
    }
    if let Some(vision) = &criteria.vision {
        vision.validate()?;
    }
    Ok(criteria)
}

/// The names of the configured criteria that only an LLM judge can score.
fn judged_criteria(criteria: &EvaluationCriteria) -> Vec<&'static str> {
    [
        ("semantic_match_score", criteria.semantic_match_score),
        ("rubric_quality_score", criteria.rubric_quality_score),
        ("safety_score", criteria.safety_score),
        ("hallucination_score", criteria.hallucination_score),
    ]
    .into_iter()
    .filter_map(|(name, threshold)| threshold.map(|_| name))
    .collect()
}

/// Builds the judge model from its id, reading the provider's API key from the environment.
fn judge_from_env(model: &str) -> Result<Arc<dyn Llm>, String> {
    let key = |var: &str| {
        std::env::var(var)
            .ok()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("judge model '{model}' needs {var} to be set"))
    };
    let judge: Arc<dyn Llm> = if model.starts_with("gemini") {
        Arc::new(adk_model::GeminiModel::from_env(model).map_err(|e| e.to_string())?)
    } else if model.starts_with("claude") {
        let config = adk_model::anthropic::AnthropicConfig::new(key("ANTHROPIC_API_KEY")?, model);
        Arc::new(adk_model::AnthropicClient::new(config).map_err(|e| e.to_string())?)
    } else if ["gpt", "o1", "o3", "o4"].iter().any(|prefix| model.starts_with(prefix)) {
        let config = adk_model::OpenAIConfig::new(key("OPENAI_API_KEY")?, model);
        Arc::new(adk_model::OpenAIClient::new(config).map_err(|e| e.to_string())?)
    } else {
        return Err(format!(
            "cannot tell which provider serves judge model '{model}'; use a gemini-*, claude-*, \
             or gpt-* model id"
        ));
    };
    Ok(judge)
}

/// `.eval-baseline.json` inside a directory eval set, or beside a single eval file.
fn default_baseline(path: &Path) -> PathBuf {
    let dir = if path.is_dir() { path } else { path.parent().unwrap_or(Path::new(".")) };
    dir.join(".eval-baseline.json")
}

/// Writes the report: with one run, the run's reports as before; with several, a
/// per-case summary of pass rates and score statistics.
fn write_report(options: &EvalOptions, reports: &[MultiRunReport]) -> Result<(), String> {
    let single: Vec<&EvaluationReport> =
        reports.iter().flat_map(|report| report.runs.first()).collect();
    let several = options.num_runs > 1;
    let results: Vec<EvaluationResult> = if several {
        reports.iter().flat_map(MultiRunReport::case_results).collect()
    } else {
        single.iter().flat_map(|report| report.results.iter().cloned()).collect()
    };
    let rendered = match options.format {
        EvalFormat::Json if several => serde_json::to_string_pretty(reports)
            .map_err(|e| format!("failed to serialize the report: {e}"))?,
        EvalFormat::Json => serde_json::to_string_pretty(&single)
            .map_err(|e| format!("failed to serialize the report: {e}"))?,
        EvalFormat::Junit => {
            let started_at =
                reports.first().map_or_else(chrono::Utc::now, |report| report.started_at);
            let combined = EvaluationReport::new("cargo-adk-eval", results, started_at);
            let suite = options
                .path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map_or("eval", |stem| stem.trim_end_matches(".test"));
            JunitReporter::generate(&combined, suite)
                .map_err(|e| format!("failed to render JUnit XML: {e}"))?
        }
        EvalFormat::Table if several => multi_run_table(reports, &results),
        EvalFormat::Table => {
            let results: Vec<&EvaluationResult> = results.iter().collect();
            let single: Vec<EvaluationReport> = single.into_iter().cloned().collect();
            table(&single, &results)
        }
    };
    match &options.output {
        Some(path) => {
            std::fs::write(path, rendered)
                .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
            eprintln!("report written to {}", path.display());
        }
        None => println!("{rendered}"),
    }
    Ok(())
}

fn table(reports: &[EvaluationReport], results: &[&EvaluationResult]) -> String {
    use std::fmt::Write;

    let passed = results.iter().filter(|r| r.passed).count();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Total cases: {}  Passed: {passed}  Failed: {}",
        results.len(),
        results.len() - passed
    );

    let mut scores: HashMap<&str, Vec<f64>> = HashMap::new();
    for result in results {
        for (criterion, &score) in &result.scores {
            scores.entry(criterion.as_str()).or_default().push(score);
        }
    }
    let mut criteria: Vec<_> = scores.keys().copied().collect();
    criteria.sort_unstable();
    if !criteria.is_empty() {
        let _ = writeln!(out, "\n{:<24} {:>8} {:>8} {:>8}", "Criterion", "Mean", "Min", "Max");
        for criterion in criteria {
            let values = &scores[criterion];
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            let min = values.iter().copied().fold(f64::INFINITY, f64::min);
            let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let _ = writeln!(out, "{criterion:<24} {mean:>8.3} {min:>8.3} {max:>8.3}");
        }
    }

    let duration: Duration = reports.iter().map(|r| r.duration).sum();
    let _ = writeln!(out, "\nDuration: {:.2}s", duration.as_secs_f64());

    for result in results.iter().filter(|r| !r.passed) {
        let _ = writeln!(out, "\nFAILED {}", result.eval_id);
        for failure in &result.failures {
            let _ = writeln!(out, "  {}", failure.format().replace('\n', "\n  "));
        }
    }
    out
}

/// One summary per eval set, then the cases that did not reach the minimum pass rate.
fn multi_run_table(reports: &[MultiRunReport], case_results: &[EvaluationResult]) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    for report in reports {
        let _ = writeln!(out, "Eval set: {}", report.eval_set_id);
        out.push_str(&report.format_summary());
    }
    for result in case_results.iter().filter(|r| !r.passed) {
        let _ = writeln!(out, "\nFAILED {}", result.eval_id);
        for failure in &result.failures {
            let _ = writeln!(out, "  {}", failure.format().replace('\n', "\n  "));
        }
    }
    out
}

// ── Agent process protocol ──────────────────────────────────────

/// One request line written to the agent's stdin.
#[derive(Debug, Serialize)]
struct AgentRequest<'a> {
    case_id: &'a str,
    turn: usize,
    user_text: &'a str,
    session_id: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    images: Vec<AgentImage>,
}

/// An image the turn shows, verified against its digest when the eval set was loaded.
#[derive(Debug, Serialize)]
struct AgentImage {
    path: String,
    digest: String,
    mime_type: String,
}

/// One response line read from the agent's stdout.
#[derive(Debug, Default, Deserialize)]
struct AgentResponse {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    tool_calls: Vec<AgentToolCall>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AgentToolCall {
    name: String,
    #[serde(default)]
    args: serde_json::Value,
    /// The tool's result, scored by the vision criteria when it is a vision tool's.
    #[serde(default)]
    response: Option<serde_json::Value>,
}

/// A running agent process and its protocol pipes.
struct AgentProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
}

impl AgentProcess {
    /// Writes one request line and reads one response line.
    async fn round_trip(&mut self, request: &AgentRequest<'_>) -> Result<AgentResponse, String> {
        let mut line = serde_json::to_string(request)
            .map_err(|e| format!("failed to encode the request: {e}"))?;
        line.push('\n');
        let written = match self.stdin.write_all(line.as_bytes()).await {
            Ok(()) => self.stdin.flush().await,
            Err(e) => Err(e),
        };
        // A process that exits before reading closes stdin first; whether the write sees the
        // closed pipe depends on timing, so stdout decides what the agent did.
        if let Err(e) = written
            && e.kind() != std::io::ErrorKind::BrokenPipe
        {
            return Err(self.exited(&format!("could not be written to ({e})")));
        }
        let reply = match self.stdout.next_line().await {
            Ok(Some(reply)) => reply,
            Ok(None) => return Err(self.exited("closed stdout before answering")),
            Err(e) => return Err(format!("failed to read the agent's stdout: {e}")),
        };
        serde_json::from_str(&reply).map_err(|e| {
            let shown: String = reply.chars().take(200).collect();
            format!(
                "the agent wrote {shown:?}, which is not a valid response line ({e}); stdout is \
                 reserved for protocol lines, so write logs to stderr"
            )
        })
    }

    /// Describes a pipe failure, adding the exit status when the process has exited.
    fn exited(&mut self, what: &str) -> String {
        match self.child.try_wait() {
            Ok(Some(status)) => format!("the agent process exited ({status}); its stdin {what}"),
            _ => format!("the agent process {what}"),
        }
    }
}

/// The agent command, started on first use and restarted after a protocol failure.
struct AgentCommand {
    command: String,
    turn_timeout: Duration,
    process: Mutex<Option<AgentProcess>>,
}

impl AgentCommand {
    fn new(command: &str, turn_timeout: Duration) -> Self {
        Self { command: command.to_string(), turn_timeout, process: Mutex::new(None) }
    }

    fn spawn(&self) -> Result<AgentProcess, String> {
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("cmd");
            command.arg("/C").arg(&self.command);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = Command::new("sh");
            command.arg("-c").arg(&self.command);
            command
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("failed to start the agent command `{}`: {e}", self.command))?;
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("the agent process has no stdin or stdout pipe".to_string());
        };
        Ok(AgentProcess { child, stdin, stdout: BufReader::new(stdout).lines() })
    }

    /// Sends one request and waits for its response.
    ///
    /// A process that times out, exits, or writes an invalid line is killed, since a late
    /// or misaligned reply would otherwise be read as the answer to the next request. The
    /// next request starts a new process.
    async fn exchange(&self, request: &AgentRequest<'_>) -> Result<AgentResponse, String> {
        let mut slot = self.process.lock().await;
        let process = match slot.as_mut() {
            Some(process) => process,
            None => slot.insert(self.spawn()?),
        };
        let outcome = tokio::time::timeout(self.turn_timeout, process.round_trip(request)).await;
        match outcome {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(e)) => {
                *slot = None;
                Err(e)
            }
            Err(_) => {
                *slot = None;
                Err(format!(
                    "the agent did not answer within {}s; raise --turn-timeout if it needs longer",
                    self.turn_timeout.as_secs_f64()
                ))
            }
        }
    }

    /// Closes the agent's stdin and waits briefly for it to exit, then kills it.
    async fn shutdown(&self) {
        let Some(AgentProcess { mut child, stdin, stdout }) = self.process.lock().await.take()
        else {
            return;
        };
        drop(stdin);
        drop(stdout);
        if tokio::time::timeout(Duration::from_secs(5), child.wait()).await.is_err() {
            let _ = child.kill().await;
        }
    }
}

/// The agent under test as the evaluator sees it: one instance per eval case.
struct CommandAgent {
    command: Arc<AgentCommand>,
    case_id: String,
    next_turn: AtomicUsize,
}

impl CommandAgent {
    fn new(command: Arc<AgentCommand>, eval_case: &EvalCase) -> Self {
        Self { command, case_id: eval_case.eval_id.clone(), next_turn: AtomicUsize::new(0) }
    }
}

#[async_trait]
impl Agent for CommandAgent {
    fn name(&self) -> &str {
        "agent_cmd"
    }

    fn description(&self) -> &str {
        "an agent process driven over the cargo adk eval protocol"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        let user_text: String = ctx.user_content().parts.iter().filter_map(Part::text).collect();
        let request = AgentRequest {
            case_id: &self.case_id,
            turn: self.next_turn.fetch_add(1, Ordering::SeqCst),
            user_text: &user_text,
            session_id: ctx.session_id(),
            images: images_of(ctx.user_content())?,
        };
        let response = self.command.exchange(&request).await.map_err(AdkError::agent)?;
        if let Some(error) = response.error {
            return Err(AdkError::agent(format!("the agent reported an error: {error}")));
        }

        let mut content = Content::new("model");
        let mut results = Content::new("function");
        for call in response.tool_calls {
            if let Some(result) = call.response {
                results.parts.push(Part::FunctionResponse {
                    function_response: adk_core::FunctionResponseData::new(&call.name, result),
                    id: None,
                    annotations: None,
                });
            }
            content.parts.push(Part::FunctionCall {
                name: call.name,
                args: call.args,
                id: None,
                thought_signature: None,
            });
        }
        if let Some(text) = response.text {
            content = content.with_text(text);
        }
        let mut events = Vec::with_capacity(2);
        for content in [content, results] {
            if content.parts.is_empty() && !events.is_empty() {
                continue;
            }
            let mut event = Event::new(ctx.invocation_id());
            event.author = self.name().to_string();
            event.llm_response.content = Some(content);
            events.push(Ok(event));
        }
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

/// The eval fixtures a turn's content shows, by path: `adk_eval` puts each verified
/// fixture's resolved path in its inline part's `adkEvalFixture` annotation.
fn images_of(content: &Content) -> adk_core::Result<Vec<AgentImage>> {
    content
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::InlineData { mime_type, data, annotations, .. } => {
                Some((mime_type, data, annotations))
            }
            _ => None,
        })
        .map(|(mime_type, data, annotations)| {
            let path = annotations
                .as_ref()
                .and_then(|a| a.pointer("/adkEvalFixture/path"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    AdkError::agent(
                        "the turn shows an image that is not an eval fixture; give it as an \
                         {\"image\": {\"path\", \"digest\"}} part",
                    )
                })?;
            Ok(AgentImage {
                path: path.to_string(),
                digest: ImageFixture::digest_of(data),
                mime_type: mime_type.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_have_the_documented_shape() {
        let request = AgentRequest {
            case_id: "weather",
            turn: 1,
            user_text: "And tomorrow?",
            session_id: "s-1",
            images: Vec::new(),
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "case_id": "weather",
                "turn": 1,
                "user_text": "And tomorrow?",
                "session_id": "s-1"
            })
        );
    }

    #[test]
    fn requests_name_each_verified_image_by_path_and_digest() {
        let mut content = Content::new("user").with_text("Find the seal.");
        content.parts.push(Part::InlineData {
            mime_type: "image/png".to_string(),
            data: b"png".to_vec(),
            uri: None,
            annotations: Some(serde_json::json!({
                "adkEvalFixture": {"path": "/evals/img/seal.png", "digest": "sha256:ignored"}
            })),
        });
        let request = AgentRequest {
            case_id: "seal",
            turn: 0,
            user_text: "Find the seal.",
            session_id: "s-2",
            images: images_of(&content).unwrap(),
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap()["images"],
            serde_json::json!([{
                "path": "/evals/img/seal.png",
                // Computed from the bytes the agent is pointed at, not copied.
                "digest": ImageFixture::digest_of(b"png"),
                "mime_type": "image/png"
            }])
        );

        let mut stray = Content::new("user");
        stray.parts.push(Part::InlineData {
            mime_type: "image/png".to_string(),
            data: b"png".to_vec(),
            uri: None,
            annotations: None,
        });
        assert!(images_of(&stray).unwrap_err().to_string().contains("not an eval fixture"));
    }

    #[test]
    fn tool_call_responses_parse() {
        let response: AgentResponse = serde_json::from_str(
            r#"{"tool_calls":[{"name":"vision_detect","args":{},"response":{"regions":[]}}]}"#,
        )
        .unwrap();
        assert_eq!(response.tool_calls[0].response, Some(serde_json::json!({"regions": []})));
    }

    #[test]
    fn vision_thresholds_are_checked_when_the_criteria_load() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("vision.json");
        std::fs::write(&file, r#"{"vision": {"mask_iou": 90}}"#).unwrap();
        assert!(load_criteria(Some(&file)).unwrap_err().contains("mask_iou is 90"));
        std::fs::write(&file, r#"{"vision": {"mask_iou": 0.9}}"#).unwrap();
        assert_eq!(load_criteria(Some(&file)).unwrap().vision.unwrap().mask_iou, Some(0.9));
    }

    #[test]
    fn responses_parse_text_tool_calls_and_errors() {
        let response: AgentResponse = serde_json::from_str(
            r#"{"text":"Sunny.","tool_calls":[{"name":"get_weather","args":{"city":"Nairobi"}}]}"#,
        )
        .unwrap();
        assert_eq!(response.text.as_deref(), Some("Sunny."));
        assert_eq!(response.tool_calls[0].name, "get_weather");
        assert_eq!(response.tool_calls[0].args, serde_json::json!({"city": "Nairobi"}));

        let response: AgentResponse = serde_json::from_str(r#"{"error":"quota"}"#).unwrap();
        assert_eq!(response.error.as_deref(), Some("quota"));

        assert!(serde_json::from_str::<AgentResponse>(r#""Sunny.""#).is_err());
    }

    #[test]
    fn criteria_that_check_nothing_are_refused() {
        let dir = std::env::temp_dir().join(format!("cargo-adk-criteria-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.json");
        std::fs::write(&empty, "{}").unwrap();
        assert!(load_criteria(Some(&empty)).unwrap_err().contains("no thresholds"));

        let custom = dir.join("custom.json");
        std::fs::write(
            &custom,
            r#"{"response_similarity": 0.8, "custom": [{"name": "tone", "description": "polite", "threshold": 0.5}]}"#,
        )
        .unwrap();
        let err = load_criteria(Some(&custom));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(err.unwrap_err().contains("custom criteria"));
    }

    #[test]
    fn default_criteria_check_tools_and_text() {
        let criteria = load_criteria(None).unwrap();
        assert_eq!(
            (criteria.tool_trajectory_score, criteria.response_similarity),
            (Some(1.0), Some(0.8))
        );
        assert!(judged_criteria(&criteria).is_empty());
    }

    #[test]
    fn judge_models_need_a_known_provider() {
        let Err(error) = judge_from_env("llama-4") else {
            panic!("an unknown judge model must be refused");
        };
        assert!(error.contains("cannot tell which provider"), "{error}");
    }
}
