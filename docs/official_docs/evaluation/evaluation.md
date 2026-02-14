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

```bash
# Basic evaluation
cargo run --example eval_basic

# Trajectory validation
cargo run --example eval_trajectory

# LLM-judged semantic matching
cargo run --example eval_semantic

# Rubric-based scoring
cargo run --example eval_rubric

# Response similarity algorithms
cargo run --example eval_similarity

# Report generation
cargo run --example eval_report
```

## Best Practices

1. **Start Simple**: Begin with trajectory validation before adding semantic checks
2. **Use Representative Cases**: Test files should cover edge cases and common scenarios
3. **Calibrate Thresholds**: Start with lenient thresholds and tighten as agent improves
4. **Combine Criteria**: Use multiple criteria for comprehensive evaluation
5. **Version Test Files**: Keep test files in version control alongside agent code
6. **CI/CD Integration**: Run evaluations in CI to catch regressions

---

**Previous**: [← A2A Protocol](../deployment/a2a.md) | **Next**: [Access Control →](../security/access-control.md)
