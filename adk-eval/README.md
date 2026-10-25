# adk-eval

Agent evaluation framework for Rust Agent Development Kit (ADK-Rust).

[![Crates.io](https://img.shields.io/crates/v/adk-eval.svg)](https://crates.io/crates/adk-eval)
[![Documentation](https://docs.rs/adk-eval/badge.svg)](https://docs.rs/adk-eval)
[![License](https://img.shields.io/crates/l/adk-eval.svg)](../LICENSE)

## Overview

`adk-eval` provides comprehensive tools for testing and validating agent behavior, enabling developers to ensure their agents perform correctly and consistently. Unlike traditional software testing, agent evaluation must account for the probabilistic nature of LLMs while still providing meaningful quality signals.

## Features

- **Test Definitions**: Structured JSON format for defining test cases (`.test.json`)
- **Trajectory Evaluation**: Validate tool call sequences with exact or partial matching
- **Response Quality**: Assess final output quality using multiple algorithms
- **LLM-Judged Evaluation**: Semantic matching, rubric-based scoring, and safety checks
- **Multiple Criteria**: Ground truth, similarity-based, and configurable thresholds
- **Detailed Reporting**: Comprehensive results with failure analysis
- **Structured LLM Judge**: Typed verdicts (pass/fail/partial) with scores and reasoning
- **Embedding Similarity**: Cosine similarity between embedding vectors (feature: `embedding`)
- **Cost & Latency Tracking**: Token usage extraction, dollar cost estimation, latency recording
- **Trace Analysis**: Detect redundant tool calls, execution loops, compute efficiency scores
- **Regression Baselines**: Save/load metric snapshots, detect quality degradation
- **Ledger Statistics**: Median/MAD baselines per dimension and day class, warm-up, windowed deviation test, and drift runs with cooldowns over a content-free activity ledger (`ledger`), also run as `cargo adk eval --ledger`
- **JUnit XML Output**: CI-friendly report generation (feature: `ci-helpers`)
- **Human Annotation**: JSONL export/import workflow for human review
- **A/B Comparison**: Statistical significance testing with Wilcoxon signed-rank (feature: `statistics`)
- **Web Experiments**: `ExperimentAnalyzer` — always-valid sequential tests with a fixed error budget for proportions and means, effect intervals, guardrail monitoring and a Bayesian option (feature: `experiments`)
- **Vertex AI Gen AI Evaluation Service**: Service-backed judge and trajectory metrics via `evaluateInstances` (feature: `vertex-eval`)
- **Test Case Generation**: LLM-driven or event-based eval case creation
- **Prompt Optimizer**: `PromptOptimizer::optimize_candidates` iterates on an agent's instruction against an eval set, evaluating the agent built from each proposal (`InstructionCandidates`)
- **User Personas**: simulated users for multi-turn evaluation (feature: `personas`)
- **Conversation Metrics**: Multi-turn scoring for context retention, goal completion, coherence, topic drift
- **CI Integration**: Eval sets run from integration tests or `cargo adk eval --agent-cmd`, with JUnit XML and baseline regression checks; `cargo adk eval --ledger` gates on drift in an exported activity ledger

## Quick Start

```toml
[dependencies]
adk-eval = "3.0.0"
adk-agent = "3.0.0"
adk-model = "3.0.0"
tokio = { version = "1", features = ["full"] }
```

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_eval::{EvaluationConfig, EvaluationCriteria, Evaluator};
use adk_model::GeminiModel;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?;
    let agent = LlmAgentBuilder::new("geography_agent")
        .instruction("Answer with the name of the city only.")
        .model(Arc::new(model))
        .build()?;

    let config = EvaluationConfig::with_criteria(EvaluationCriteria::response_similarity(0.8));
    let evaluator = Evaluator::new(config);

    let report = evaluator.evaluate_file(Arc::new(agent), "tests/geography.test.json").await?;
    println!("{}", report.format_summary());
    assert!(report.all_passed());
    Ok(())
}
```

`tests/geography.test.json`:

```json
{
  "eval_set_id": "geography",
  "name": "Geography",
  "description": "Capital cities",
  "eval_cases": [
    {
      "eval_id": "capital_of_france",
      "conversation": [
        {
          "invocation_id": "inv_001",
          "user_content": { "parts": [{ "text": "What is the capital of France?" }], "role": "user" },
          "final_response": { "parts": [{ "text": "Paris" }], "role": "model" }
        }
      ]
    }
  ]
}
```

## Test File Format

A test file with an expected tool call:

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

The turns of a case run in order through one session of an in-memory session service, so each turn sees the history and state of the turns before it. The case's `session_input` sets the session's `app_name`, `user_id` (default `eval_app` and `eval_user`), and initial `state`.

## Evaluation Criteria

### Tool Trajectory Matching

Validates that the agent calls expected tools:

```rust
use adk_eval::{EvaluationCriteria, ToolTrajectoryConfig};

fn main() {
    let criteria = EvaluationCriteria {
        tool_trajectory_score: Some(1.0), // require a full match
        tool_trajectory_config: Some(ToolTrajectoryConfig {
            strict_order: true, // tools must be called in order
            strict_args: false, // extra arguments are allowed
        }),
        ..Default::default()
    };
    assert!(criteria.has_criteria());
}
```

Arguments are compared recursively. With `strict_args: false` (the default), every expected key must be present with a matching value at any depth and extra keys are allowed; with `strict_args: true`, objects must have the same keys at every depth. Arrays match element by element, numbers compare by value (`1` matches `1.0`), and an expected tool use without `args` matches any arguments.

### Response Similarity

Compare response text using various algorithms:

```rust
use adk_eval::criteria::SimilarityAlgorithm;
use adk_eval::{EvaluationCriteria, ResponseMatchConfig};

fn main() {
    let criteria = EvaluationCriteria {
        response_similarity: Some(0.8), // 80% similarity required
        response_match_config: Some(ResponseMatchConfig {
            algorithm: SimilarityAlgorithm::Jaccard, // word overlap
            ignore_case: true,
            normalize: true,
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(criteria.has_criteria());
}
```

Available similarity algorithms:
- `Exact` - Exact string match
- `Contains` - Substring check
- `Levenshtein` - Edit distance
- `Jaccard` - Word overlap (default)
- `Rouge1` - Unigram overlap
- `Rouge2` - Bigram overlap
- `RougeL` - Longest common subsequence

### LLM-Judged Semantic Matching

Use an LLM to judge semantic equivalence between expected and actual responses:

```rust
use std::sync::Arc;

use adk_eval::{EvaluationConfig, EvaluationCriteria, Evaluator};
use adk_model::GeminiModel;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let judge_model = Arc::new(GeminiModel::new(std::env::var("GOOGLE_API_KEY")?, "gemini-3.7-flash")?);
    let config = EvaluationConfig::with_criteria(
        EvaluationCriteria::semantic_match(0.85), // 85% semantic similarity required
    );
    let evaluator = Evaluator::with_llm_judge(config, judge_model);
    assert!(evaluator.has_llm_judge());
    Ok(())
}
```

`LlmJudgeConfig` sets the judge's `max_tokens` and `temperature`; both default to `None`, which
leaves them to the provider, because Claude 5 and OpenAI reasoning models reject sampling
parameters. `LlmJudge::with_config(model, config)` applies them.

### Rubric-Based Evaluation

Evaluate responses against custom rubrics:

```rust
use adk_eval::{EvaluationCriteria, Rubric};

fn main() {
    let criteria = EvaluationCriteria::default().with_rubrics(
        0.7,
        vec![
            Rubric::new("Accuracy", "Response is factually correct").with_weight(0.5),
            Rubric::new("Helpfulness", "Response addresses user's needs").with_weight(0.3),
            Rubric::new("Clarity", "Response is clear and well-organized").with_weight(0.2),
        ],
    );
    assert_eq!(criteria.rubric_quality_score, Some(0.7));
}
```

### Safety and Hallucination Detection

Check responses for safety issues and hallucinations:

```rust
use adk_eval::EvaluationCriteria;

fn main() {
    let criteria = EvaluationCriteria {
        safety_score: Some(0.95),       // require a high safety score
        hallucination_score: Some(0.9), // require a low hallucination rate
        ..Default::default()
    };
    assert!(criteria.has_criteria());
}
```

An `EQUIVALENT: NO`, `SAFE: NO`, or `HALLUCINATION_FREE: NO` verdict fails the criterion regardless of its score, and a judge reply without the verdict line is a judge error.

### Criteria That Cannot Be Judged

LLM-judged criteria fail the case with a score of 0.0, rather than being skipped, when no LLM judge is configured, the agent produced no text response, `rubric_quality_score` is set without rubrics, or the judge reply lacks a valid `SCORE:` line. A structured judge `fail` verdict also fails the case. A criterion's case score is the arithmetic mean of its per-turn scores, and an agent stream error fails the case with an `execution` failure.

## Result Reporting

```rust
use std::sync::Arc;

use adk_core::Agent;
use adk_eval::Evaluator;

async fn report(evaluator: &Evaluator, agent: Arc<dyn Agent>) -> Result<(), Box<dyn std::error::Error>> {
    let report = evaluator.evaluate_file(agent, "tests/agent.test.json").await?;

    println!("Total: {}", report.summary.total);
    println!("Passed: {}", report.summary.passed);
    println!("Failed: {}", report.summary.failed);
    println!("Pass rate: {:.1}%", report.summary.pass_rate * 100.0);

    for result in report.failures() {
        println!("Failed: {}", result.eval_id);
        for failure in &result.failures {
            println!("  - {}", failure.format());
        }
    }

    let json = report.to_json()?;
    println!("{json}");
    Ok(())
}
```

## Batch Evaluation

Evaluate multiple test cases in parallel (results follow the order of `cases`; a concurrency of 0 or no cases is a `ConfigError`):

```rust
use std::sync::Arc;

use adk_core::Agent;
use adk_eval::{EvalCase, Evaluator};

async fn run_cases(evaluator: &Evaluator, agent: Arc<dyn Agent>, cases: &[EvalCase]) -> adk_eval::Result<()> {
    let results = evaluator.evaluate_cases_parallel(agent.clone(), cases, 4).await?; // 4 concurrent
    for result in results {
        let result = result?;
        println!("{}: passed={}", result.eval_id, result.passed);
    }

    // Every *.test.json file in a directory.
    let reports = evaluator.evaluate_directory(agent, "tests/eval_cases").await?;
    println!("{} files", reports.len());
    Ok(())
}
```

A case passes only when at least one criterion was evaluated and none failed: an empty test file, a case without turns, an evaluator without criteria, or a directory without `*.test.json` files returns `EvalError::ConfigError`, and a case no criterion applies to fails as `not_evaluated`. `EvaluationReport::all_passed()` is `false` for a report without results.

## Integration with cargo test

```rust
use std::sync::Arc;

use adk_agent::LlmAgentBuilder;
use adk_eval::{EvaluationConfig, EvaluationCriteria, Evaluator};
use adk_model::GeminiModel;

#[tokio::test]
#[ignore = "calls the Gemini API"]
async fn geography_agent_passes_its_eval_set() {
    let model = GeminiModel::new(std::env::var("GOOGLE_API_KEY").unwrap(), "gemini-3.7-flash").unwrap();
    let agent = LlmAgentBuilder::new("geography_agent")
        .instruction("Answer with the name of the city only.")
        .model(Arc::new(model))
        .build()
        .unwrap();
    let evaluator =
        Evaluator::new(EvaluationConfig::with_criteria(EvaluationCriteria::response_similarity(0.8)));

    let report = evaluator.evaluate_file(Arc::new(agent), "tests/geography.test.json").await.unwrap();

    assert!(report.all_passed(), "{}", report.format_summary());
}
```

## Advanced Features

### Feature Flags

```toml
[dependencies]
adk-eval = { version = "3.0.0", features = ["embedding", "ci-helpers", "statistics"] }
```

| Feature | Dependency | Capability |
|---------|-----------|------------|
| `embedding` | `adk-memory` | Embedding-based semantic similarity |
| `ci-helpers` | `quick-xml` | JUnit XML report generation |
| `statistics` | `statrs` | Wilcoxon signed-rank for A/B comparison |
| `vertex-eval` | `adk-gcp` | Vertex AI Gen AI Evaluation Service bridge (`VertexEvalClient`, `VertexEvalJudge`) |
| `personas` | none | The `personas` module: simulated users for multi-turn evaluation |
| `experiments` | `libm` | `ExperimentAnalyzer`: sequential analysis of web experiments with guardrails |

All other features (structured judge, cost tracker, trace analyzer, baselines, annotations, test generator, conversation scorer) work without extra feature flags.

### Structured LLM Judge

```rust
use std::sync::Arc;

use adk_core::Llm;
use adk_eval::{StructuredJudge, Verdict};

async fn judge(model: Arc<dyn Llm>) -> adk_eval::Result<()> {
    let judge = StructuredJudge::new(model);
    let verdict = judge.judge("Paris", "The capital is Paris.", "accuracy").await?;
    println!("{:?} {:.2}: {}", verdict.verdict, verdict.score, verdict.reasoning);
    assert!(matches!(verdict.verdict, Verdict::Pass | Verdict::Partial | Verdict::Fail));
    Ok(())
}
```

### Cost and Latency Tracking

```rust
use adk_eval::CostTracker;

fn main() {
    let tracker = CostTracker::new();
    // gemini-3.7-flash: $0.00075 per 1K input tokens, $0.00375 per 1K output tokens.
    let cost = tracker.compute_cost("gemini-3.7-flash", 2000, 800).unwrap();
    assert!((cost - 0.0045).abs() < 1e-9);
}
```

`tracker.extract_metrics(&events, duration)` returns token counts, `cost_usd`, and latency from a
run's events.

### Execution Trace Analysis

```rust
use adk_core::Event;
use adk_eval::TraceAnalyzer;

fn efficiency(events: &[Event]) {
    let analysis = TraceAnalyzer::new().analyze(events);
    println!("efficiency: {:.0}%", analysis.efficiency_score * 100.0);
    for diagnostic in &analysis.diagnostics {
        println!("{diagnostic:?}");
    }
}
```

### Regression Baselines

```rust
use std::collections::HashMap;

use adk_eval::BaselineStore;

fn main() -> adk_eval::Result<()> {
    // metric name -> case id -> score
    let metrics = HashMap::from([(
        "response_similarity".to_string(),
        HashMap::from([("capital_of_france".to_string(), 1.0)]),
    )]);

    let store = BaselineStore::new(std::env::temp_dir().join("eval-baseline.json"));
    store.save("geography", &metrics)?;
    let regressions = store.check_regressions(&metrics, 0.05)?;
    assert!(regressions.is_empty());
    Ok(())
}
```

A baseline metric or case with no score in the new run, such as a case that errored, is a regression with `current_value: None`. A missing baseline file is an error rather than an empty result.

### Ledger Statistics

`adk_eval::ledger` compares the last 7 and 14 days of each dimension of an activity ledger with the median and MAD of the 28 days before them, per weekday and weekend class, and reports a deviation when `|recent − median| > max(2.5 × MAD, floor)` after a 14-day warm-up. Two deviating dimensions, or one for 10 days, are a meaningful drift. Everything is a pure function, with no I/O and no model.

```rust
use adk_eval::ledger::{DeviationConfig, assess, daily_series, parse_jsonl};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let export = std::fs::read_to_string("ledger.jsonl")?;
    let series = daily_series(&parse_jsonl(&export)?);
    let config = DeviationConfig::default().with_floor("tool_call.duration_ms", 50.0);
    if let Some(assessment) = assess(&series, &config) {
        assert!(!assessment.meaningful, "drift: {:?}", assessment.runs);
    }
    Ok(())
}
```

`cargo adk eval --ledger ledger.jsonl` runs the same assessment as a CI gate; see the [evaluation guide](../docs/official_docs/evaluation/evaluation.md#gating-on-ledger-drift-with-cargo-adk-eval---ledger) for the export format, the dimensions and their floors.

### JUnit XML (CI Integration)

```rust
use adk_eval::{EvaluationReport, JunitReporter}; // requires the ci-helpers feature

fn junit(report: &EvaluationReport) -> adk_eval::Result<String> {
    JunitReporter::generate(report, "my_suite")
}
```

### Human Annotation Workflow

```rust
use std::collections::HashSet;

use adk_eval::{AnnotationStore, EvalCase, EvaluationResult};

fn review(cases: &[EvalCase], results: &[EvaluationResult]) -> adk_eval::Result<()> {
    AnnotationStore::export(cases, results, "review.jsonl")?;

    let valid_ids: HashSet<String> = cases.iter().map(|case| case.eval_id.clone()).collect();
    let (records, warnings) = AnnotationStore::import("review.jsonl", &valid_ids)?;
    println!("{} records, {} warnings", records.len(), warnings.len());
    Ok(())
}
```

### A/B Agent Comparison

```rust
use std::sync::Arc;

use adk_core::Agent;
use adk_eval::{AbComparator, EvalCase, Evaluator}; // requires the statistics feature

async fn compare(
    evaluator: Evaluator,
    agent_a: Arc<dyn Agent>,
    agent_b: Arc<dyn Agent>,
    cases: &[EvalCase],
) -> adk_eval::Result<()> {
    let report = AbComparator::new(evaluator).compare(agent_a, agent_b, cases).await?;
    println!("{} vs {} over {} cases", report.agent_a_name, report.agent_b_name, report.total_cases);
    Ok(())
}
```

### Web Experiments

`ExperimentAnalyzer` (feature `experiments`) analyses an experiment that splits visitors between a control and a variant, look by look, without spending more than its error budget however often it is consulted. Guardrails are checked first at every look; a proven harm beyond a guardrail's tolerance stops the experiment and keeps the control.

```rust
use adk_eval::experiments::{
    Decision, ExperimentAnalyzer, ExperimentConfig, GuardrailSpec, Look, MetricDirection,
    MetricSpec, Sample, Tolerance,
};

fn analyse(looks: &[Look]) -> adk_eval::Result<Decision> {
    let config = ExperimentConfig::new(
        "headline",
        MetricSpec::proportion("signups", MetricDirection::HigherIsBetter),
    )
    .with_guardrail(GuardrailSpec::new(
        MetricSpec::proportion("unsubscribes", MetricDirection::LowerIsBetter),
        Tolerance::Relative(0.1),
    ));
    let mut analyzer = ExperimentAnalyzer::new(config)?;
    let mut decision = Decision::Continue;
    for look in looks {
        // Each look carries the cumulative totals of every metric in both arms.
        decision = analyzer.observe(look)?.decision;
        if decision.is_final() {
            break;
        }
    }
    Ok(decision)
}

fn main() -> adk_eval::Result<()> {
    let look = Look::new()
        .with_metric("signups", Sample::proportion(600, 24), Sample::proportion(600, 30))
        .with_metric("unsubscribes", Sample::proportion(600, 3), Sample::proportion(600, 6));
    assert_eq!(analyse(&[look])?, Decision::Continue);
    Ok(())
}
```

See [Web Experiments](../docs/official_docs/evaluation/experiments.md) for the method, its assumptions and the simulation results.

### Auto-Generated Test Cases

```rust
use std::sync::Arc;

use adk_core::{Event, Llm};
use adk_eval::TestGenerator;

async fn generate(model: Arc<dyn Llm>, production_events: &[Event]) -> adk_eval::Result<()> {
    let generator = TestGenerator::new(model);
    let from_description = generator.generate_from_description("A weather assistant").await?;
    let from_events = generator.generate_from_events(production_events)?;
    println!("{} + {} cases", from_description.len(), from_events.len());
    Ok(())
}
```

### Multi-Turn Conversation Metrics

```rust
use adk_core::Content;
use adk_eval::{ConversationScorer, StructuredJudge};

async fn score(judge: StructuredJudge, conversation: &[Content]) -> adk_eval::Result<()> {
    let metrics = ConversationScorer::new(judge).score(conversation, "book a flight").await?;
    println!(
        "retention {:.2}, goal {:.2}, coherence {:.2}, drift {:.2}",
        metrics.context_retention, metrics.goal_completion, metrics.coherence, metrics.topic_drift
    );
    Ok(())
}
```

### Running Evaluations in CI

Run eval sets from an integration test, where the agent is constructed, and fail on case failures or baseline regressions. See "Running Evaluations in CI" in the [evaluation guide](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/evaluation/evaluation.md) for a complete test with JUnit XML output.

`cargo adk eval` runs an eval set against an agent binary that reads one JSON request per line on stdin (`{"case_id","turn","user_text","session_id"}`) and writes one JSON response per line on stdout (`{"text","tool_calls":[{"name","args"}]}` or `{"error"}`). It exits non-zero when a case fails or a score regresses:

```bash
cargo adk eval tests/ --agent-cmd "target/release/eval_agent" --check-regression --format junit --output results.xml
```

See "Running an Agent Binary with `cargo adk eval`" in the [evaluation guide](https://github.com/zavora-ai/adk-rust/blob/main/docs/official_docs/evaluation/evaluation.md) for the protocol, flags, and an example agent binary.

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
