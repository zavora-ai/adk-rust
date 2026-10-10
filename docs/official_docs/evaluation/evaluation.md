# Agent Evaluation

The `adk-eval` crate provides comprehensive tools for testing and validating agent behavior. Unlike traditional software testing, agent evaluation must account for the probabilistic nature of LLMs while still providing meaningful quality signals.

## Overview

Agent evaluation in ADK-Rust supports multiple evaluation strategies:

- **Trajectory Evaluation**: Validate that agents call expected tools in the correct sequence
- **Response Similarity**: Compare agent responses using various algorithms (Jaccard, Levenshtein, ROUGE)
- **LLM-Judged Evaluation**: Use another LLM to assess semantic similarity and quality
- **Rubric-Based Scoring**: Evaluate against custom criteria with weighted scoring

## Quick Start

```rust
use adk_eval::{Evaluator, EvaluationConfig, EvaluationCriteria};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create your agent
    let agent = create_my_agent()?;

    // Configure evaluator with criteria
    let config = EvaluationConfig::with_criteria(
        EvaluationCriteria::exact_tools()
            .with_response_similarity(0.8)
    );

    let evaluator = Evaluator::new(config);

    // Run evaluation against test file
    let report = evaluator
        .evaluate_file(agent, "tests/my_agent.test.json")
        .await?;

    // Check results
    if report.all_passed() {
        println!("All {} tests passed!", report.summary.total);
    } else {
        println!("{}", report.format_summary());
    }

    Ok(())
}
```

## Test File Format

Test cases are defined in JSON files with the `.test.json` extension:

```json
{
  "eval_set_id": "weather_agent_tests",
  "name": "Weather Agent Tests",
  "description": "Test weather agent functionality",
  "eval_cases": [
    {
      "eval_id": "test_current_weather",
      "conversation": [
        {
          "invocation_id": "inv_001",
          "user_content": {
            "parts": [{"text": "What's the weather in NYC?"}],
            "role": "user"
          },
          "final_response": {
            "parts": [{"text": "The weather in NYC is 65°F and sunny."}],
            "role": "model"
          },
          "intermediate_data": {
            "tool_uses": [
              {
                "name": "get_weather",
                "args": {"location": "NYC"}
              }
            ]
          }
        }
      ]
    }
  ]
}
```

### Multi-Turn Cases and Session Input

The turns of a case run in order through one session, using a `Runner` over a private in-memory session service, so each turn sees the conversation history and state that earlier turns left. Each case starts a new session, built from its `session_input`:

| `session_input` field | Use | Default when empty |
|-----------------------|-----|--------------------|
| `app_name` | The session's app name | `eval_app` |
| `user_id` | The session's user id | `eval_user` |
| `state` | The session's initial state | No state |

```json
{
  "eval_id": "remembers_the_city",
  "session_input": {"app_name": "weather", "user_id": "ada", "state": {"user:units": "metric"}},
  "conversation": [
    {"invocation_id": "inv_1", "user_content": {"parts": [{"text": "I live in Nairobi."}]}},
    {"invocation_id": "inv_2", "user_content": {"parts": [{"text": "Weather at home?"}]},
     "intermediate_data": {"tool_uses": [{"name": "get_weather", "args": {"location": "Nairobi"}}]}}
  ]
}
```

## Evaluation Criteria

### Tool Trajectory Matching

Validates that agents call expected tools in the correct order:

```rust
let criteria = EvaluationCriteria {
    tool_trajectory_score: Some(1.0),  // Require 100% match
    tool_trajectory_config: Some(ToolTrajectoryConfig {
        strict_order: true,   // Tools must be called in exact order
        strict_args: false,   // Allow extra arguments in tool calls
    }),
    ..Default::default()
};
```

**Options:**
- `strict_order` (default `true`): Require expected calls in order. `ToolTrajectoryScorer::score` and `ToolTrajectoryScorer::compare` both apply it.
- `strict_args` (default `false`): Require exact arguments instead of a subset match
- Partial matching with configurable thresholds

Expected and actual arguments are compared recursively:

| Arguments | `strict_args: false` | `strict_args: true` |
|-----------|----------------------|---------------------|
| Objects | Every expected key present with a matching value, at any depth; extra keys allowed | Same keys at every depth |
| Arrays | Same length, matched element by element | Same length, matched element by element |
| Numbers | Compared by value (`1` matches `1.0`) | Compared by value |
| `args` omitted from the expected tool use | Matches any arguments | Matches any arguments |

### Response Similarity

Compare response text using various algorithms:

```rust
let criteria = EvaluationCriteria {
    response_similarity: Some(0.8),  // 80% similarity required
    response_match_config: Some(ResponseMatchConfig {
        algorithm: SimilarityAlgorithm::Jaccard,
        ignore_case: true,
        normalize: true,
        ..Default::default()
    }),
    ..Default::default()
};
```

**Available algorithms:**
| Algorithm | Description |
|-----------|-------------|
| `Exact` | Exact string match |
| `Contains` | Substring check |
| `Levenshtein` | Edit distance |
| `Jaccard` | Word overlap (default) |
| `Rouge1` | Unigram overlap |
| `Rouge2` | Bigram overlap |
| `RougeL` | Longest common subsequence |

### LLM-Judged Semantic Matching

Use an LLM to evaluate semantic equivalence:

```rust
use adk_eval::{Evaluator, EvaluationConfig, EvaluationCriteria, LlmJudge};
use adk_model::GeminiModel;

// Create evaluator with LLM judge
let judge_model = Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?);
let config = EvaluationConfig::with_criteria(
    EvaluationCriteria::semantic_match(0.85)
);
let evaluator = Evaluator::with_llm_judge(config, judge_model);
```

The LLM judge assesses:
- Semantic equivalence (same meaning, different words)
- Factual accuracy
- Completeness of response

The judge replies with an `EQUIVALENT: YES/NO/PARTIAL` verdict and a `SCORE:`. An `EQUIVALENT: NO` verdict fails the criterion whatever the score; `PARTIAL` is judged on the score. A `SemanticMatchConfig::custom_prompt` must ask for both lines.

Judge requests carry no temperature and use the provider's default output limit, so models that reject sampling parameters (Claude 5 models, OpenAI reasoning models) can judge. `LlmJudge::with_config` takes an `LlmJudgeConfig { temperature, max_tokens }`; `Some` values are sent on every judge request. `StructuredJudgeConfig::temperature` behaves the same way.

### Rubric-Based Evaluation

Evaluate against custom criteria with weighted scoring:

```rust
use adk_eval::{Rubric, EvaluationCriteria};

let criteria = EvaluationCriteria::default()
    .with_rubrics(0.7, vec![
        Rubric::new("Accuracy", "Response is factually correct")
            .with_weight(0.5),
        Rubric::new("Helpfulness", "Response addresses user's needs")
            .with_weight(0.3),
        Rubric::new("Clarity", "Response is clear and well-organized")
            .with_weight(0.2),
    ]);
```

Each rubric is scored 0-1 by the LLM judge, then combined using weights.

### Safety and Hallucination Detection

Check responses for safety issues and hallucinations. Both criteria need an LLM judge:

```rust
let criteria = EvaluationCriteria {
    safety_score: Some(0.95),        // Require high safety score
    hallucination_score: Some(0.9),  // Require low hallucination rate
    ..Default::default()
};
let evaluator = Evaluator::with_llm_judge(EvaluationConfig::with_criteria(criteria), judge_model);
```

A `SAFE: NO` or `HALLUCINATION_FREE: NO` verdict from the judge fails the criterion regardless of its score.

### Criteria That Cannot Be Judged

`semantic_match_score`, `rubric_quality_score`, `safety_score`, and `hallucination_score` fail the case, with a score of 0.0, when they cannot be judged. The failure details state the reason:

| Condition | Failure details |
|-----------|-----------------|
| No LLM judge (`Evaluator::new` without `set_llm_judge`) | `this criterion needs an LLM judge; ...` |
| The agent produced no text response | `the agent produced no text response to judge` |
| `rubric_quality_score` set without rubrics | `rubric_quality_score is set but rubric_config has no rubrics; ...` |
| The judge reply lacks a valid `SCORE:` line (a number from 0.0 to 1.0), or an `EQUIVALENT:` / `SAFE:` / `HALLUCINATION_FREE:` line for those criteria | `LLM judge error: judge response has no valid SCORE line ...` |
| The judge call fails | `LLM judge error: ...` |

`semantic_match_score` skips turns that have no expected `final_response`, as `response_similarity` does.

### Scores Across Turns and Errors

A criterion's case score is the arithmetic mean of its per-turn scores. When the agent fails to start or its event stream yields an error, the case fails with an `execution` failure and carries no scores.

## Result Reporting

The evaluation report provides detailed results:

```rust
let report = evaluator.evaluate_file(agent, "tests/agent.test.json").await?;

// Summary statistics
println!("Total: {}", report.summary.total);
println!("Passed: {}", report.summary.passed);
println!("Failed: {}", report.summary.failed);
println!("Pass Rate: {:.1}%", report.summary.pass_rate * 100.0);

// Detailed failures
for result in report.failures() {
    println!("Failed: {}", result.eval_id);
    for failure in &result.failures {
        println!("  - {}: {} (expected: {}, actual: {})",
            failure.criterion,
            failure.message,
            failure.expected,
            failure.actual
        );
    }
}

// Export to JSON for CI/CD
let json = report.to_json()?;
std::fs::write("eval_results.json", json)?;
```

## Batch Evaluation

### Parallel Evaluation

Evaluate multiple test cases concurrently:

```rust
let results = evaluator
    .evaluate_cases_parallel(agent, &cases, 4)  // 4 concurrent evaluations
    .await;
```

### Directory Evaluation

Evaluate all test files in a directory:

```rust
let reports = evaluator
    .evaluate_directory(agent, "tests/eval_cases")
    .await?;

for (file, report) in reports {
    println!("{}: {} passed, {} failed",
        file,
        report.summary.passed,
        report.summary.failed
    );
}
```

## Integration with cargo test

Use evaluation in standard Rust tests:

```rust
#[tokio::test]
async fn test_weather_agent() {
    let agent = create_weather_agent().unwrap();
    let evaluator = Evaluator::new(EvaluationConfig::with_criteria(
        EvaluationCriteria::exact_tools()
    ));

    let report = evaluator
        .evaluate_file(agent, "tests/weather_agent.test.json")
        .await
        .unwrap();

    assert!(report.all_passed(), "{}", report.format_summary());
}
```

## Examples

Use the standard feature tier for evaluation APIs:

```bash
cargo check -p adk-rust --no-default-features --features standard
```

The runnable evaluation gallery is maintained in [adk-playground](https://github.com/zavora-ai/adk-playground).

## Advanced Features

The following capabilities bring `adk-eval` to parity with frameworks like Braintrust, LangSmith, and Inspect AI. They are additive to the existing API and feature-gated where they introduce new dependencies.

### Structured LLM Judge

Produces typed verdicts (pass/fail/partial) with scores and reasoning via function-calling or JSON fallback:

```rust
use adk_eval::{StructuredJudge, StructuredJudgeConfig};

let judge = StructuredJudge::new(model.clone());

let verdict = judge.judge(
    "The capital of France is Paris.",
    "Paris is the capital of France.",
    "factual_accuracy"
).await?;

println!("Score: {:.2}, Verdict: {:?}", verdict.score, verdict.verdict);
println!("Reasoning: {}", verdict.reasoning);
```

The judge attempts function-calling (response schema) first, then falls back to prompting for JSON with a lenient parser that handles raw JSON, markdown fences, and embedded JSON in prose.

`Evaluator::set_structured_judge` runs the judge on the last turn's expected and actual responses. A `fail` verdict, a judge error, or a missing agent response fails the case; `pass` and `partial` verdicts record the score without failing it.

### Cost and Latency Tracking

Track token usage and compute estimated dollar costs per evaluation:

```rust
use adk_eval::{CostTracker, CostMetrics};

let tracker = CostTracker::new();  // Uses default pricing tables

// Compute cost for a known model
let cost = tracker.compute_cost("gpt-4o", 2000, 800);
// → Some(0.013)

// Extract metrics from event streams
let metrics = tracker.extract_metrics(&events, duration);
println!("Tokens: {}, Latency: {}ms", metrics.total_tokens, metrics.latency_ms);
```

### Execution Trace Analysis

Detect redundant tool calls, execution loops, and compute efficiency scores:

```rust
use adk_eval::{TraceAnalyzer, TraceAnalysis};

let analyzer = TraceAnalyzer::new();
let analysis = analyzer.analyze(&events);

println!("Efficiency: {:.1}%", analysis.efficiency_score * 100.0);
for diag in &analysis.diagnostics {
    println!("  [{:?}] {}", diag.pattern_type, diag.description);
}
```

### Regression Baselines

Save evaluation metrics as baselines and detect quality regressions:

```rust
use adk_eval::BaselineStore;

let store = BaselineStore::new(".eval-baseline.json");

// Save current metrics (metric name → case id → score)
store.save("my_eval_set", &metrics)?;

// On next run, check for regressions
let regressions = store.check_regressions(&current_metrics, 0.05)?;
for reg in &regressions {
    match reg.current_value {
        Some(current) => println!("REGRESSION: {} [{}] dropped from {:.3} to {:.3}",
            reg.metric_name, reg.case_id, reg.baseline_value, current),
        None => println!("REGRESSION: {} [{}] has no score in this run",
            reg.metric_name, reg.case_id),
    }
}
```

Every metric and case recorded in the baseline must have a score in the current run. A case that errored or did not run has no scores, so it is reported as a regression with `current_value: None`.

A missing baseline file is an error (`EvalError::BaselineError`), not an empty list, so a mistyped path or an uncommitted baseline cannot pass the gate. Save one with `BaselineStore::save` first.

### CI Output (JUnit XML)

Generate JUnit XML for native CI integration (GitHub Actions, Jenkins, GitLab CI):

```rust
use adk_eval::JunitReporter;  // requires `ci-helpers` feature

let xml = JunitReporter::generate(&report, "my_eval_suite")?;
std::fs::write("test-results.xml", xml)?;
```

### Human Annotation Workflow

Export cases for human review and import verdicts back:

```rust
use adk_eval::AnnotationStore;

// Export cases for annotation
AnnotationStore::export(&cases, &results, "review.jsonl")?;

// After human review, import back
let (records, warnings) = AnnotationStore::import("review.jsonl", &valid_ids)?;
```

### A/B Agent Comparison

Compare two agents with statistical significance testing:

```rust
use adk_eval::{AbComparator, ab_comparator::wilcoxon_signed_rank};

// Requires `statistics` feature
let comparator = AbComparator::new(evaluator);
let report = comparator.compare(agent_a, agent_b, &eval_cases).await?;

for cmp in &report.criteria_comparisons {
    println!("{}: A={:.3} B={:.3} p={:.4} significant={}",
        cmp.criterion, cmp.agent_a_mean, cmp.agent_b_mean,
        cmp.p_value, cmp.significant);
}
```

### Auto-Generated Test Cases

Generate evaluation cases from descriptions (via LLM) or production event logs:

```rust
use adk_eval::{TestGenerator, GeneratorConfig};

let generator = TestGenerator::with_config(model, GeneratorConfig {
    cases_per_description: 5,
    include_tool_expectations: true,
});

// From natural language
let cases = generator.generate_from_description(
    "A weather assistant that looks up forecasts by city"
).await?;

// From production events (no LLM needed)
let cases = generator.generate_from_events(&production_events)?;
```

### Multi-Turn Conversation Metrics

Evaluate extended conversations on four dimensions:

```rust
use adk_eval::{ConversationScorer, ConversationScorerConfig};

let scorer = ConversationScorer::new(judge);
let metrics = scorer.score(&conversation, "Help user plan a trip").await?;

println!("Context retention: {:.2}", metrics.context_retention);
println!("Goal completion:   {:.2}", metrics.goal_completion);
println!("Coherence:         {:.2}", metrics.coherence);
println!("Topic drift:       {:.2}", metrics.topic_drift);
```

### Embedding-Based Semantic Similarity

Measure meaning preservation using vector embeddings (requires `embedding` feature):

```rust
use adk_eval::EmbeddingScorer;

let scorer = EmbeddingScorer::new(embedding_provider);
let score = scorer.score("expected text", "actual text").await?;
// Returns 0.0–1.0 cosine similarity
```

### Feature Flags

| Feature | Dependency | Capability |
|---------|-----------|------------|
| `embedding` | `adk-memory` | Embedding-based semantic similarity |
| `ci-helpers` | `quick-xml` | JUnit XML report generation |
| `statistics` | `statrs` | Wilcoxon signed-rank test for A/B comparison |

All other features (structured judge, cost tracker, trace analyzer, baselines, annotations, test generator, conversation scorer) work without any additional feature flags.

## Running Evaluations in CI

Run eval sets from an integration test, where the agent under test is constructed. The test fails when any case fails or any baseline metric regresses, and writes JUnit XML for the CI test reporter.

```toml
[dev-dependencies]
adk-core = "3.0.0"
adk-eval = { version = "3.0.0", features = ["ci-helpers"] }
tokio = { version = "1", features = ["macros", "rt"] }
```

```rust
// tests/eval.rs
use std::collections::HashMap;
use std::sync::Arc;

use adk_core::Agent;
use adk_eval::{BaselineStore, EvaluationConfig, EvaluationCriteria, Evaluator, JunitReporter};

/// Builds the agent under test the same way the application does.
fn build_agent() -> Arc<dyn Agent> {
    my_app::build_weather_agent()
}

#[tokio::test]
async fn weather_agent_meets_its_eval_set() {
    let evaluator = Evaluator::new(EvaluationConfig::with_criteria(
        EvaluationCriteria::exact_tools().with_response_similarity(0.8),
    ));
    let report = evaluator
        .evaluate_file(build_agent(), "tests/weather_agent.test.json")
        .await
        .expect("eval set loads");

    let xml = JunitReporter::generate(&report, "weather_agent").expect("JUnit XML renders");
    std::fs::write("target/eval-results.xml", xml).expect("JUnit XML is written");

    // Metric name → case id → score. Errored cases have no scores and so regress.
    let mut metrics: HashMap<String, HashMap<String, f64>> = HashMap::new();
    for result in &report.results {
        for (metric, score) in &result.scores {
            metrics.entry(metric.clone()).or_default().insert(result.eval_id.clone(), *score);
        }
    }
    let store = BaselineStore::new("tests/.eval-baseline.json");
    if std::env::var_os("SAVE_EVAL_BASELINE").is_some() {
        store.save("weather_agent", &metrics).expect("baseline is saved");
    }
    let regressions = store.check_regressions(&metrics, 0.05).expect("baseline exists and is readable");

    assert!(report.all_passed(), "{}", report.format_summary());
    assert!(regressions.is_empty(), "regressions: {regressions:#?}");
}
```

1. Run `SAVE_EVAL_BASELINE=1 cargo test --test eval` once and commit `tests/.eval-baseline.json`.
2. Run `cargo test --test eval` in CI and publish `target/eval-results.xml` with the CI test reporter.

### Running an Agent Binary with `cargo adk eval`

`cargo adk eval` cannot construct a Rust agent, so it runs your agent as a separate process and talks to it over a line protocol. `--agent-cmd` starts the process once per run; the CLI writes one JSON request per turn to its stdin and reads one JSON response from its stdout.

| Direction | Line |
|-----------|------|
| Request (stdin) | `{"case_id": "current_weather", "turn": 0, "user_text": "Weather in Nairobi?", "session_id": "…"}` |
| Response (stdout) | `{"text": "Sunny, 24°C.", "tool_calls": [{"name": "get_weather", "args": {"city": "Nairobi"}}]}` |
| Error response (stdout) | `{"error": "model quota exhausted"}` |

- **`turn`** is the zero-based index of the turn within the case.
- **`session_id`** is the same for every turn of a case and different for each case, so key conversation state by it.
- **stdout** carries protocol lines only; write logs to stderr, which the CLI passes through.
- **Failures** — an `error` response, an invalid line, an exited process, or no answer within `--turn-timeout` — fail the case with an `execution` failure. After any failure other than an `error` response, the CLI starts a new process for the next request.

```bash
cargo adk eval tests/ --agent-cmd "target/release/eval_agent" --check-regression --format junit --output results.xml
```

| Flag | Meaning | Default |
|------|---------|---------|
| `--agent-cmd` | Command that starts the agent, run with `sh -c` (`cmd /C` on Windows) | Required |
| `--criteria` | `EvaluationCriteria` JSON file | `tool_trajectory_score` 1.0 and `response_similarity` 0.8 |
| `--judge-model` | Judge for LLM-judged criteria: `gemini-*` (`GOOGLE_API_KEY`), `claude-*` (`ANTHROPIC_API_KEY`), or `gpt-*`/`o1`/`o3`/`o4` (`OPENAI_API_KEY`) | None; LLM-judged criteria are refused without it |
| `--save-baseline` | Save this run's scores; refused when a case did not run to completion | Off |
| `--check-regression` | Compare against the baseline; a missing baseline is an error | Off |
| `--baseline` | Baseline file | `.eval-baseline.json` in the eval set's directory |
| `--tolerance` | Largest score drop that is not a regression | `0.05` |
| `--format` | `table`, `json`, or `junit` | `table` |
| `--output` | Write the report to a file instead of stdout | stdout |
| `--turn-timeout` | Seconds to wait for each response | `300` |

The command exits with status 0 only when every case passed and no score regressed. A criteria file that sets no threshold, or sets `custom` criteria, which `Evaluator` does not score, is refused before any case runs.

A Rust agent binary that speaks the protocol, built from the same agent as the application:

```toml
[dependencies]
adk-rust = "3.0.0"
anyhow = "1"
serde_json = "1"
tokio = { version = "1", features = ["io-std", "io-util", "macros", "rt-multi-thread"] }
```

```rust
// src/bin/eval_agent.rs
use std::collections::HashMap;
use std::sync::Arc;

use adk_rust::futures::StreamExt;
use adk_rust::prelude::*;
use adk_rust::session::{CreateRequest, SessionService};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("GOOGLE_API_KEY")?;
    let agent = LlmAgentBuilder::new("weather_agent")
        .model(Arc::new(GeminiModel::new(&api_key, "gemini-3.7-flash")?))
        .instruction("You answer weather questions.")
        .build()?;
    let sessions = Arc::new(InMemorySessionService::new());
    let runner = Runner::builder()
        .app_name("eval")
        .agent(Arc::new(agent))
        .session_service(sessions.clone())
        .build()?;

    // Each eval case has its own session_id; give each one its own runner session.
    let mut runner_sessions: HashMap<String, String> = HashMap::new();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        let request: Value = serde_json::from_str(&line)?;
        let eval_session = request["session_id"].as_str().unwrap_or_default().to_string();
        let session_id = match runner_sessions.get(&eval_session) {
            Some(session_id) => session_id.clone(),
            None => {
                let session = sessions
                    .create(CreateRequest {
                        app_name: "eval".to_string(),
                        user_id: "eval".to_string(),
                        session_id: None,
                        state: HashMap::new(),
                    })
                    .await?;
                let session_id = session.id().to_string();
                runner_sessions.insert(eval_session, session_id.clone());
                session_id
            }
        };

        let input = Content::new("user").with_text(request["user_text"].as_str().unwrap_or_default());
        let response = match run_turn(&runner, &session_id, input).await {
            Ok((text, tool_calls)) => json!({ "text": text, "tool_calls": tool_calls }),
            Err(error) => json!({ "error": error.to_string() }),
        };
        stdout.write_all(format!("{response}\n").as_bytes()).await?;
        stdout.flush().await?;
    }
    Ok(())
}

/// Runs one turn, returning the final text and the tool calls the model made.
async fn run_turn(runner: &Runner, session_id: &str, input: Content) -> Result<(String, Vec<Value>)> {
    let mut events = runner.run_str("eval", session_id, input).await?;
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    while let Some(event) = events.next().await {
        let event = event?;
        if event.llm_response.partial {
            continue;
        }
        for part in event.content().map(|content| content.parts.iter()).into_iter().flatten() {
            if let Some(chunk) = part.text() {
                text.push_str(chunk);
            }
            if let Part::FunctionCall { name, args, .. } = part {
                tool_calls.push(json!({ "name": name, "args": args }));
            }
        }
    }
    Ok((text, tool_calls))
}
```

The protocol is language-neutral. A Python agent needs only a read loop:

```python
#!/usr/bin/env python3
import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    reply = {"text": f"You said: {request['user_text']}", "tool_calls": []}
    print(json.dumps(reply), flush=True)
```

## Best Practices

1. **Start Simple**: Begin with trajectory validation before adding semantic checks
2. **Use Representative Cases**: Test files should cover edge cases and common scenarios
3. **Calibrate Thresholds**: Start with lenient thresholds and tighten as agent improves
4. **Combine Criteria**: Use multiple criteria for comprehensive evaluation
5. **Version Test Files**: Keep test files in version control alongside agent code
6. **CI/CD Integration**: Run evaluations in CI to catch regressions
7. **Save Baselines**: Save a baseline after establishing a quality bar, then check for regressions in CI (see [Running Evaluations in CI](#running-evaluations-in-ci))
8. **Use Structured Judges**: Prefer `StructuredJudge` over plain LLM judge for machine-parseable results
9. **Track Costs**: Enable `CostTracker` to monitor efficiency regressions alongside quality
10. **Detect Loops**: Enable `TraceAnalyzer` to catch agents stuck in repetitive patterns

## Example

A complete working example demonstrating all features is available:

```bash
cargo run --manifest-path examples/eval_showcase/Cargo.toml
```

See `examples/eval_showcase/` for the source code.

---

**Previous**: [← A2A Protocol](../deployment/a2a.md) | **Next**: [Access Control →](../security/access-control.md)
