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

## License

Apache-2.0

## Part of ADK-Rust

This crate is part of the [ADK-Rust](https://adk-rust.com) framework for building AI agents in Rust.
