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
- `strict_order`: Require exact sequence matching
- `strict_args`: Require exact argument matching (no extra args allowed)
- Partial matching with configurable thresholds

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
let judge_model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);
let config = EvaluationConfig::with_criteria(
    EvaluationCriteria::semantic_match(0.85)
);
let evaluator = Evaluator::with_llm_judge(config, judge_model);
```

The LLM judge assesses:
- Semantic equivalence (same meaning, different words)
- Factual accuracy
- Completeness of response

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

Check responses for safety issues and hallucinations:

```rust
let criteria = EvaluationCriteria {
    safety_score: Some(0.95),        // Require high safety score
    hallucination_score: Some(0.9),  // Require low hallucination rate
    ..Default::default()
};
```

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

// Save current metrics
store.save("my_eval_set", &metrics)?;

// On next run, check for regressions
let regressions = store.check_regressions(&current_metrics, 0.05)?;
if !regressions.is_empty() {
    for reg in &regressions {
        println!("REGRESSION: {} dropped from {:.3} to {:.3}",
            reg.metric_name, reg.baseline_value, reg.current_value);
    }
}
```

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

## CLI Integration

Run evaluations from the command line via `cargo adk eval`:

```bash
# Basic evaluation
cargo adk eval tests/my_agent.test.json

# Save baseline
cargo adk eval tests/ --save-baseline

# Check for regressions
cargo adk eval tests/ --check-regression --tolerance 0.05

# JUnit XML output for CI
cargo adk eval tests/ --format junit --output results.xml

# JSON output
cargo adk eval tests/ --format json

# Parallel execution
cargo adk eval tests/ --concurrency 4
```

Exit codes:
- `0` — all evaluations passed, no regressions
- `1` — regressions detected (when `--check-regression` is set)

## Best Practices

1. **Start Simple**: Begin with trajectory validation before adding semantic checks
2. **Use Representative Cases**: Test files should cover edge cases and common scenarios
3. **Calibrate Thresholds**: Start with lenient thresholds and tighten as agent improves
4. **Combine Criteria**: Use multiple criteria for comprehensive evaluation
5. **Version Test Files**: Keep test files in version control alongside agent code
6. **CI/CD Integration**: Run evaluations in CI to catch regressions
7. **Save Baselines**: Use `--save-baseline` after establishing a quality bar, then `--check-regression` in CI
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
