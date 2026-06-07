# adk-eval

Agent evaluation framework for Rust Agent Development Kit (ADK-Rust).

[![Crates.io](https://img.shields.io/crates/v/adk-eval.svg)](https://crates.io/crates/adk-eval)
[![Documentation](https://docs.rs/adk-eval/badge.svg)](https://docs.rs/adk-eval)
[![License](https://img.shields.io/crates/l/adk-eval.svg)](LICENSE)

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
- **JUnit XML Output**: CI-friendly report generation (feature: `ci-helpers`)
- **Human Annotation**: JSONL export/import workflow for human review
- **A/B Comparison**: Statistical significance testing with Wilcoxon signed-rank (feature: `statistics`)
- **Test Case Generation**: LLM-driven or event-based eval case creation
- **Conversation Metrics**: Multi-turn scoring for context retention, goal completion, coherence, topic drift
- **CLI Integration**: `cargo adk eval` with baselines, regression checks, and parallel execution

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

    // Run evaluation
    let report = evaluator
        .evaluate_file(agent, "tests/my_agent.test.json")
        .await?;

    // Check results
    if report.all_passed() {
        println!("All tests passed!");
    } else {
        println!("{}", report.format_summary());
    }

    Ok(())
}
```

## Test File Format

Test files use JSON format with the following structure:

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

Validates that the agent calls expected tools:

```rust
let criteria = EvaluationCriteria {
    tool_trajectory_score: Some(1.0),  // Require 100% match
    tool_trajectory_config: Some(ToolTrajectoryConfig {
        strict_order: true,   // Tools must be called in order
        strict_args: false,   // Allow extra arguments
    }),
    ..Default::default()
};
```

### Response Similarity

Compare response text using various algorithms:

```rust
let criteria = EvaluationCriteria {
    response_similarity: Some(0.8),  // 80% similarity required
    response_match_config: Some(ResponseMatchConfig {
        algorithm: SimilarityAlgorithm::Jaccard,  // Word overlap
        ignore_case: true,
        normalize: true,
        ..Default::default()
    }),
    ..Default::default()
};
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
use adk_eval::{Evaluator, EvaluationConfig, EvaluationCriteria, LlmJudge};
use adk_model::GeminiModel;
use std::sync::Arc;

// Create evaluator with LLM judge
let judge_model = Arc::new(GeminiModel::new(&api_key, "gemini-2.5-flash")?);
let config = EvaluationConfig::with_criteria(
    EvaluationCriteria::semantic_match(0.85)  // 85% semantic similarity required
);
let evaluator = Evaluator::with_llm_judge(config, judge_model);
```

### Rubric-Based Evaluation

Evaluate responses against custom rubrics:

```rust
use adk_eval::{Rubric, RubricConfig, EvaluationCriteria};

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

```rust
let report = evaluator.evaluate_file(agent, "tests/agent.test.json").await?;

// Summary
println!("Total: {}", report.summary.total);
println!("Passed: {}", report.summary.passed);
println!("Failed: {}", report.summary.failed);
println!("Pass Rate: {:.1}%", report.summary.pass_rate * 100.0);

// Detailed failures
for result in report.failures() {
    println!("Failed: {}", result.eval_id);
    for failure in &result.failures {
        println!("  - {}", failure.format());
    }
}

// Export to JSON
let json = report.to_json()?;
```

## Batch Evaluation

Evaluate multiple test cases in parallel:

```rust
let results = evaluator
    .evaluate_cases_parallel(agent, &cases, 4)  // 4 concurrent
    .await;
```

Evaluate all test files in a directory:

```rust
let reports = evaluator
    .evaluate_directory(agent, "tests/eval_cases")
    .await?;
```

## Integration with cargo test

```rust
#[tokio::test]
async fn test_my_agent() {
    let agent = create_my_agent().unwrap();
    let evaluator = Evaluator::new(EvaluationConfig::with_criteria(
        EvaluationCriteria::exact_tools()
    ));

    let report = evaluator
        .evaluate_file(agent, "tests/my_agent.test.json")
        .await
        .unwrap();

    assert!(report.all_passed(), "{}", report.format_summary());
}
```

## Advanced Features

### Feature Flags

```toml
[dependencies]
adk-eval = { version = "0.10", features = ["embedding", "ci-helpers", "statistics"] }
```

| Feature | Dependency | Capability |
|---------|-----------|------------|
| `embedding` | `adk-memory` | Embedding-based semantic similarity |
| `ci-helpers` | `quick-xml` | JUnit XML report generation |
| `statistics` | `statrs` | Wilcoxon signed-rank for A/B comparison |

All other features (structured judge, cost tracker, trace analyzer, baselines, annotations, test generator, conversation scorer) work without extra feature flags.

### Structured LLM Judge

```rust
use adk_eval::StructuredJudge;

let judge = StructuredJudge::new(model);
let verdict = judge.judge("expected", "actual", "accuracy").await?;
// → StructuredVerdict { score: 0.85, verdict: Partial, reasoning: "..." }
```

### Cost and Latency Tracking

```rust
use adk_eval::CostTracker;

let tracker = CostTracker::new();
let cost = tracker.compute_cost("gpt-4o", 2000, 800); // → Some($0.013)
let metrics = tracker.extract_metrics(&events, duration);
```

### Execution Trace Analysis

```rust
use adk_eval::TraceAnalyzer;

let analyzer = TraceAnalyzer::new();
let analysis = analyzer.analyze(&events);
println!("Efficiency: {:.0}%", analysis.efficiency_score * 100.0);
```

### Regression Baselines

```rust
use adk_eval::BaselineStore;

let store = BaselineStore::new(".eval-baseline.json");
store.save("my_eval", &metrics)?;
let regressions = store.check_regressions(&new_metrics, 0.05)?;
```

### JUnit XML (CI Integration)

```rust
use adk_eval::JunitReporter;  // requires ci-helpers feature

let xml = JunitReporter::generate(&report, "my_suite")?;
```

### Human Annotation Workflow

```rust
use adk_eval::AnnotationStore;

AnnotationStore::export(&cases, &results, "review.jsonl")?;
let (records, warnings) = AnnotationStore::import("review.jsonl", &valid_ids)?;
```

### A/B Agent Comparison

```rust
use adk_eval::AbComparator;  // requires statistics feature

let comparator = AbComparator::new(evaluator);
let report = comparator.compare(agent_a, agent_b, &cases).await?;
```

### Auto-Generated Test Cases

```rust
use adk_eval::TestGenerator;

let gen = TestGenerator::new(model);
let cases = gen.generate_from_description("A weather assistant").await?;
let cases = gen.generate_from_events(&production_events)?;
```

### Multi-Turn Conversation Metrics

```rust
use adk_eval::ConversationScorer;

let scorer = ConversationScorer::new(judge);
let metrics = scorer.score(&conversation, "goal").await?;
// → ConversationMetrics { context_retention, goal_completion, coherence, topic_drift }
```

### CLI

```bash
cargo adk eval tests/ --save-baseline
cargo adk eval tests/ --check-regression --format junit --output results.xml
```

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
