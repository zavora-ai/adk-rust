# Agent Evaluation Framework (Roadmap)

> **Status**: Not yet implemented  
> **Priority**: High  
> **Est. Effort**: 4-6 weeks

## Overview

The Agent Evaluation Framework will provide comprehensive tools for testing and validating agent behavior, enabling developers to ensure their agents perform correctly and consistently. Unlike traditional software testing, agent evaluation must account for the probabilistic nature of LLMs while still providing meaningful quality signals.

## The Problem

Testing AI agents presents unique challenges:

- **Non-Determinism**: LLM responses vary between runs
- **Complex Behavior**: Agents make multi-step decisions
- **Tool Usage**: Need to validate tool call sequences
- **Quality Metrics**: Traditional pass/fail insufficient
- **Regression Testing**: Changes can subtly break behavior

## Planned Solution

The evaluation framework will provide:

- **Test Definitions**: Structured format for defining test cases
- **Trajectory Evaluation**: Validate tool call sequences
- **Response Quality**: Assess final output quality
- **Multiple Criteria**: Ground truth, rubric-based, and LLM-judged metrics
- **Automation**: Run evaluations in CI/CD pipelines
- **Reporting**: Detailed results and failure analysis

## Planned Architecture

### Evaluation Components

```
┌─────────────────────────────────────────────────────────┐
│                  Evaluation Framework                    │
├─────────────────────────────────────────────────────────┤
│                                                          │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐ │
│  │  Test Files  │  │  Eval Sets   │  │   Criteria   │ │
│  │  (.test.json)│  │ (.evalset)   │  │   Config     │ │
│  └──────────────┘  └──────────────┘  └──────────────┘ │
│         │                  │                  │         │
│         └──────────────────┴──────────────────┘         │
│                           │                             │
│                    ┌──────▼──────┐                      │
│                    │  Evaluator  │                      │
│                    └──────┬──────┘                      │
│                           │                             │
│         ┌─────────────────┼─────────────────┐          │
│         │                 │                 │          │
│    ┌────▼────┐      ┌────▼────┐      ┌────▼────┐     │
│    │Tool Use │      │Response │      │ Quality │     │
│    │Validator│      │ Matcher │      │ Scorer  │     │
│    └─────────┘      └─────────┘      └─────────┘     │
│                                                         │
│                    ┌──────────────┐                    │
│                    │   Reporter   │                    │
│                    └──────────────┘                    │
└─────────────────────────────────────────────────────────┘
```

## Implementation Plan

### Phase 1: Core Infrastructure (Week 1-2)
- [ ] Define test file schema (`.test.json`)
- [ ] Define eval set schema (`.evalset.json`)
- [ ] Create `Evaluator` struct
- [ ] Implement test case execution
- [ ] Basic result reporting

### Phase 2: Evaluation Criteria (Week 2-3)
- [ ] Tool trajectory matching
- [ ] Response similarity (ROUGE-based)
- [ ] LLM-judged semantic matching
- [ ] Rubric-based evaluation
- [ ] Custom criteria support

### Phase 3: Advanced Features (Week 3-4)
- [ ] User simulation for dynamic conversations
- [ ] Batch evaluation
- [ ] Parallel test execution
- [ ] Result caching
- [ ] Regression detection

### Phase 4: Integration & Tooling (Week 4-5)
- [ ] CLI tool (`adk-eval`)
- [ ] Test framework integration (cargo test)
- [ ] Web UI for test management
- [ ] CI/CD integration examples
- [ ] Migration tools

### Phase 5: Documentation & Examples (Week 5-6)
- [ ] Comprehensive documentation
- [ ] Example test suites
- [ ] Best practices guide
- [ ] Tutorial videos
- [ ] API reference

## Test File Format

### Basic Test File

```json
{
  "eval_set_id": "weather_agent_tests",
  "name": "Weather Agent Unit Tests",
  "description": "Test basic weather agent functionality",
  "eval_cases": [
    {
      "eval_id": "test_current_weather",
      "conversation": [
        {
          "invocation_id": "inv_001",
          "user_content": {
            "parts": [{"text": "What's the weather in San Francisco?"}],
            "role": "user"
          },
          "final_response": {
            "parts": [{"text": "The current weather in San Francisco is 65°F and sunny."}],
            "role": "model"
          },
          "intermediate_data": {
            "tool_uses": [
              {
                "name": "get_weather",
                "args": {
                  "location": "San Francisco",
                  "unit": "fahrenheit"
                }
              }
            ],
            "intermediate_responses": []
          }
        }
      ],
      "session_input": {
        "app_name": "weather_agent",
        "user_id": "test_user",
        "state": {}
      }
    }
  ]
}
```

### Multi-Turn Conversation

```json
{
  "eval_id": "test_multi_turn",
  "conversation": [
    {
      "invocation_id": "inv_001",
      "user_content": {
        "parts": [{"text": "What's the weather in Tokyo?"}],
        "role": "user"
      },
      "final_response": {
        "parts": [{"text": "Tokyo is currently 20°C and cloudy."}],
        "role": "model"
      },
      "intermediate_data": {
        "tool_uses": [
          {"name": "get_weather", "args": {"location": "Tokyo"}}
        ]
      }
    },
    {
      "invocation_id": "inv_002",
      "user_content": {
        "parts": [{"text": "How about tomorrow?"}],
        "role": "user"
      },
      "final_response": {
        "parts": [{"text": "Tomorrow in Tokyo will be 22°C and partly sunny."}],
        "role": "model"
      },
      "intermediate_data": {
        "tool_uses": [
          {"name": "get_forecast", "args": {"location": "Tokyo", "days": 1}}
        ]
      }
    }
  ]
}
```

## Evaluation Criteria

### Tool Trajectory Matching

Exact match of tool call sequence:

```rust,ignore
pub struct ToolTrajectoryScore {
    /// Threshold for passing (0.0 - 1.0)
    pub threshold: f64,
}

// Usage
let criteria = EvaluationCriteria {
    tool_trajectory_avg_score: Some(1.0),  // Require 100% match
    ..Default::default()
};
```

### Response Similarity

ROUGE-based text similarity:

```rust,ignore
pub struct ResponseMatchScore {
    /// Threshold for passing (0.0 - 1.0)
    pub threshold: f64,
    /// ROUGE variant (rouge-1, rouge-2, rouge-l)
    pub variant: RougeVariant,
}

// Usage
let criteria = EvaluationCriteria {
    response_match_score: Some(0.8),  // 80% similarity
    ..Default::default()
};
```

### LLM-Judged Semantic Match

Use LLM to judge semantic equivalence:

```rust,ignore
pub struct FinalResponseMatchV2 {
    /// Model to use for judging
    pub judge_model: String,
    /// Threshold for passing
    pub threshold: f64,
}

// Usage
let criteria = EvaluationCriteria {
    final_response_match_v2: Some(FinalResponseMatchV2 {
        judge_model: "gemini-2.0-flash-exp".to_string(),
        threshold: 0.9,
    }),
    ..Default::default()
};
```

### Rubric-Based Evaluation

Custom rubrics for quality assessment:

```rust,ignore
pub struct RubricBasedQuality {
    pub rubrics: Vec<Rubric>,
}

pub struct Rubric {
    pub name: String,
    pub description: String,
    pub weight: f64,
}

// Usage
let criteria = EvaluationCriteria {
    rubric_based_final_response_quality_v1: Some(RubricBasedQuality {
        rubrics: vec![
            Rubric {
                name: "Conciseness".to_string(),
                description: "Response is brief and to the point".to_string(),
                weight: 0.3,
            },
            Rubric {
                name: "Accuracy".to_string(),
                description: "Information is factually correct".to_string(),
                weight: 0.7,
            },
        ],
    }),
    ..Default::default()
};
```

## API Design

### Evaluator

```rust,ignore
use adk_eval::{Evaluator, EvaluationConfig, EvaluationResult};

pub struct Evaluator {
    config: EvaluationConfig,
}

impl Evaluator {
    pub fn new(config: EvaluationConfig) -> Self {
        Self { config }
    }
    
    pub async fn evaluate_file(
        &self,
        agent: Arc<dyn Agent>,
        test_file: &Path,
    ) -> Result<EvaluationResult> {
        // Load test file
        // Execute each test case
        // Apply evaluation criteria
        // Return results
    }
    
    pub async fn evaluate_set(
        &self,
        agent: Arc<dyn Agent>,
        eval_set: &Path,
    ) -> Result<Vec<EvaluationResult>> {
        // Load eval set
        // Execute all eval cases
        // Apply criteria
        // Return aggregated results
    }
}
```

### Test Execution

```rust,ignore
pub struct TestCase {
    pub eval_id: String,
    pub conversation: Vec<Turn>,
    pub session_input: SessionInput,
}

pub struct Turn {
    pub invocation_id: String,
    pub user_content: Content,
    pub expected_response: Content,
    pub expected_tool_uses: Vec<ToolUse>,
}

pub async fn execute_test_case(
    agent: Arc<dyn Agent>,
    test_case: &TestCase,
) -> Result<TestResult> {
    // Create session
    // Execute each turn
    // Collect actual responses and tool uses
    // Compare with expected
}
```

### Result Reporting

```rust,ignore
pub struct EvaluationResult {
    pub eval_id: String,
    pub passed: bool,
    pub scores: HashMap<String, f64>,
    pub failures: Vec<Failure>,
    pub duration: Duration,
}

pub struct Failure {
    pub criterion: String,
    pub expected: Value,
    pub actual: Value,
    pub score: f64,
    pub threshold: f64,
}
```

## Usage Examples

### Programmatic Evaluation

```rust,ignore
use adk_eval::{Evaluator, EvaluationConfig, EvaluationCriteria};

#[tokio::test]
async fn test_weather_agent() -> Result<()> {
    // Create agent
    let agent = create_weather_agent()?;
    
    // Configure evaluator
    let evaluator = Evaluator::new(EvaluationConfig {
        criteria: EvaluationCriteria {
            tool_trajectory_avg_score: Some(1.0),
            response_match_score: Some(0.8),
            ..Default::default()
        },
        ..Default::default()
    });
    
    // Run evaluation
    let result = evaluator.evaluate_file(
        agent,
        Path::new("tests/weather_agent.test.json"),
    ).await?;
    
    // Assert results
    assert!(result.passed, "Evaluation failed: {:?}", result.failures);
    
    Ok(())
}
```

### CLI Evaluation

```bash
# Evaluate single test file
adk-eval run \
    --agent ./my_agent \
    --test tests/basic.test.json \
    --criteria config/criteria.json

# Evaluate eval set
adk-eval run \
    --agent ./my_agent \
    --evalset tests/comprehensive.evalset.json \
    --detailed

# Run specific tests from eval set
adk-eval run \
    --agent ./my_agent \
    --evalset tests/all.evalset.json:test1,test2,test3
```

### Cargo Test Integration

```rust,ignore
// tests/agent_evaluation.rs
use adk_eval::Evaluator;

#[tokio::test]
async fn evaluate_agent_suite() {
    let agent = create_agent().unwrap();
    let evaluator = Evaluator::default();
    
    // Run all test files in directory
    let results = evaluator
        .evaluate_directory(agent, "tests/eval_cases")
        .await
        .unwrap();
    
    // Check all passed
    for result in results {
        assert!(
            result.passed,
            "Test {} failed: {:?}",
            result.eval_id,
            result.failures
        );
    }
}
```

## User Simulation

For dynamic conversations where responses vary:

```json
{
  "eval_id": "test_booking_flow",
  "scenario": {
    "description": "User wants to book a flight",
    "user_goal": "Book a round-trip flight from NYC to LAX",
    "max_turns": 10
  },
  "criteria": {
    "hallucinations_v1": 0.9,
    "safety_v1": 0.95
  }
}
```

```rust,ignore
pub struct UserSimulation {
    pub scenario: Scenario,
    pub simulator_model: String,
}

pub struct Scenario {
    pub description: String,
    pub user_goal: String,
    pub max_turns: usize,
}

// Simulator generates user responses dynamically
let simulation = UserSimulation {
    scenario: Scenario {
        description: "User wants to book a flight".to_string(),
        user_goal: "Book round-trip NYC to LAX".to_string(),
        max_turns: 10,
    },
    simulator_model: "gemini-2.0-flash-exp".to_string(),
};
```

## Web UI Integration

### Test Management

- Create and edit test cases interactively
- Save conversations as test cases
- Organize tests into eval sets
- Configure evaluation criteria

### Execution

- Run evaluations from UI
- View real-time progress
- Inspect detailed results
- Compare runs over time

### Analysis

- Side-by-side comparison of expected vs actual
- Tool call trajectory visualization
- Score breakdowns by criterion
- Failure pattern analysis

## CI/CD Integration

### GitHub Actions Example

```yaml
name: Agent Evaluation

on: [push, pull_request]

jobs:
  evaluate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      
      - name: Setup Rust
        uses: actions-rs/toolchain@v1
        
      - name: Run Evaluations
        env:
          GOOGLE_API_KEY: ${{ secrets.GOOGLE_API_KEY }}
        run: |
          cargo test --test agent_evaluation
          
      - name: Upload Results
        uses: actions/upload-artifact@v2
        with:
          name: evaluation-results
          path: target/eval-results/
```

## Best Practices

### Test Design

1. **Start Simple**: Begin with basic single-turn tests
2. **Cover Edge Cases**: Test error conditions and edge cases
3. **Multi-Turn**: Test conversation continuity
4. **Tool Sequences**: Validate complex tool call patterns
5. **State Management**: Test state persistence across turns

### Criteria Selection

1. **Unit Tests**: Use `tool_trajectory_avg_score` and `response_match_score`
2. **Integration Tests**: Use `final_response_match_v2` for semantic matching
3. **Quality Tests**: Use rubric-based evaluation for subjective quality
4. **Safety Tests**: Always include `safety_v1` for production agents

### Maintenance

1. **Version Tests**: Version test files with agent versions
2. **Update Regularly**: Update expected responses as agent improves
3. **Track Metrics**: Monitor evaluation scores over time
4. **Regression Detection**: Alert on score decreases

## Comparison with adk-go

ADK-Go (Python) has comprehensive evaluation support with:
- Test file and eval set formats
- Multiple evaluation criteria
- Web UI for test management
- CLI tool for automation
- Vertex AI Evaluation Service integration

ADK-Rust will achieve feature parity with these capabilities.

## Timeline

The evaluation framework is planned for a future release. This is a high-priority feature for production readiness.

Key milestones:
1. Core evaluation infrastructure
2. Criteria implementations
3. CLI and test integration
4. Web UI integration
5. Documentation and examples
6. Production deployment patterns

## Contributing

If you're interested in contributing to the evaluation framework, please:

1. Review the existing agent and session code
2. Familiarize yourself with evaluation concepts
3. Check the ADK-Go (Python) implementation
4. Open an issue to discuss your approach

---

**Related**:
- [Testing Strategy in Design Doc](../official_docs/design.md#testing-strategy)
- [Sessions Documentation](../official_docs/sessions/sessions.md)
- [Events Documentation](../official_docs/events/events.md)

**Note**: This is a roadmap document. The APIs and examples shown here are illustrative and subject to change during implementation.
