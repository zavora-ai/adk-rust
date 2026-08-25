# Vertex AI Gen AI Evaluation Service

The `vertex-eval` feature bridges adk-eval to the Vertex AI Gen AI Evaluation
Service. Model-based judgments run on the service's autorater instead of a
local LLM, and tool trajectories are scored by the service's
computation-based trajectory metrics. Every call is a single POST to
`projects.locations:evaluateInstances` (v1beta1).

## Setup

```toml
[dependencies]
adk-eval = { version = "2.1.0", features = ["vertex-eval"] }
```

Authentication uses Application Default Credentials
(`gcloud auth application-default login`, or the workload identity of a
deployed container). The caller needs the `aiplatform.endpoints.predict`
permission (`roles/aiplatform.user`).

| Environment variable | Purpose |
|----------------------|---------|
| `GOOGLE_CLOUD_PROJECT` | GCP project for `VertexEvalConfig::from_env` |
| `GOOGLE_CLOUD_LOCATION` | Region, e.g. `us-central1` |

## Service-backed judge

`VertexEvalJudge` mirrors `LlmJudge`'s evaluation surface — same method names,
same result types — so it drops into code written against the local judge:

```rust
use adk_eval::{VertexEvalClient, VertexEvalConfig, VertexEvalJudge};
use adk_eval::criteria::{Rubric, RubricConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = VertexEvalConfig::from_env()?;
    let judge = VertexEvalJudge::new(VertexEvalClient::new_with_adc(config)?);

    // Semantic equivalence (pointwiseMetricInput under the hood)
    let result = judge
        .semantic_match("The capital is Paris", "Paris is the capital of France", None)
        .await?;
    println!("score={} equivalent={}", result.score, result.equivalent);

    // Rubric-based quality, weight-normalized like LlmJudge
    let rubrics = RubricConfig {
        rubrics: vec![
            Rubric::new("Accuracy", "Response is factually correct").with_weight(2.0),
            Rubric::new("Clarity", "Response is easy to follow"),
        ],
    };
    let quality = judge.evaluate_rubrics("agent output", "task context", &rubrics).await?;
    println!("overall={}", quality.overall_score);

    // Safety and hallucination checks
    let safety = judge.evaluate_safety("agent output").await?;
    let hallucination = judge
        .detect_hallucinations("agent output", "provided context", Some("ground truth"))
        .await?;
    println!("safe={} grounded={}", safety.is_safe, hallucination.hallucination_free);
    Ok(())
}
```

Differences from `LlmJudge`, both consequences of the service returning one
`{score, explanation}` pair per judgment:

- Boolean verdicts (`equivalent`, `is_safe`, `hallucination_free`) are derived
  from the score — at or above 0.5 counts as a pass.
- `issues` carries the service's explanation as a single entry instead of a
  parsed list.

## Trajectory metrics

`VertexEvalClient::evaluate_trajectory` maps adk-eval `ToolUse` values onto
the wire `Trajectory` shape (`name` → `toolName`, `args` → JSON-encoded
`toolInput`) and returns the score:

| `TrajectoryMetric` | Meaning |
|--------------------|---------|
| `ExactMatch` | 1 if the trajectories match exactly, else 0 |
| `InOrderMatch` | 1 if all reference tool calls appear in order, else 0 |
| `AnyOrderMatch` | 1 if all reference tool calls appear in any order, else 0 |
| `Precision` | Average precision of the predicted tool calls |
| `Recall` | Average recall of the reference tool calls |

```rust
use adk_eval::{TrajectoryMetric, VertexEvalClient, VertexEvalConfig};
use adk_eval::schema::ToolUse;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = VertexEvalClient::new_with_adc(VertexEvalConfig::from_env()?)?;

    let predicted = vec![ToolUse::new("get_weather").with_args(json!({ "city": "Paris" }))];
    let reference = predicted.clone();

    let score = client
        .evaluate_trajectory(TrajectoryMetric::ExactMatch, &predicted, &reference)
        .await?;
    assert_eq!(score, 1.0);
    Ok(())
}
```

## Judge model configuration

`AutoraterConfig` selects the judge model and sampling for model-based
metrics; the server ignores it for computation-based metrics:

```rust
use adk_eval::{AutoraterConfig, VertexEvalClient, VertexEvalConfig};

fn build() -> adk_core::Result<VertexEvalClient> {
    let client = VertexEvalClient::new_with_adc(VertexEvalConfig::from_env()?)?
        .with_autorater_config(
            AutoraterConfig::new()
                .with_autorater_model(
                    "projects/p/locations/us-central1/publishers/google/models/gemini-3.7-flash",
                )
                .with_sampling_count(1),
        );
    Ok(client)
}
```

## Custom metrics

`evaluate_pointwise` takes any `PointwiseMetricSpec` — the
`metricPromptTemplate` contains `{placeholder}` variables rendered
server-side from the instance object:

```rust
use adk_eval::{PointwiseMetricSpec, VertexEvalClient, VertexEvalConfig};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = VertexEvalClient::new_with_adc(VertexEvalConfig::from_env()?)?;

    let spec = PointwiseMetricSpec::new(
        "Rate the politeness of the response from 0.0 to 1.0.\n\nResponse:\n{response}",
    );
    let result = client
        .evaluate_pointwise(&spec, &json!({ "response": "Thanks for asking!" }))
        .await?;
    println!("score={:?} explanation={:?}", result.score, result.explanation);
    Ok(())
}
```

`evaluate_instances` is the raw escape hatch: it POSTs any
`EvaluateInstancesRequest` body and returns the raw response, reaching every
other metric the service supports (BLEU, ROUGE, pairwise, tool-call metrics).

## Error handling

Errors are structured `AdkError` values with component `eval` and
`eval.vertex.*` codes (`eval.vertex.rate_limited`, `eval.vertex.unauthorized`,
`eval.vertex.invalid_response`, ...). `VertexEvalJudge` methods return the
crate's `EvalError::JudgeError`, matching `LlmJudge`.

## See also

- [Agent Evaluation](evaluation.md) — the evaluator, criteria, and local judges
- [Vertex AI Gen AI evaluation overview](https://cloud.google.com/vertex-ai/generative-ai/docs/models/evaluation-overview)
- [`projects.locations.evaluateInstances` REST reference](https://cloud.google.com/vertex-ai/generative-ai/docs/reference/rest/v1beta1/projects.locations/evaluateInstances)
