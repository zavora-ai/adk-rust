//! Contract tests for the Gen AI Evaluation Service bridge against a mock
//! server: pointwise judgments, trajectory metrics, and autorater config.
//!
//! The captured request bodies are compared as whole JSON values — they are
//! the `projects.locations:evaluateInstances` wire contract the Vertex AI
//! Gen AI Evaluation Service and python-aiplatform share.

#![cfg(feature = "vertex-eval")]

use adk_eval::criteria::{Rubric, RubricConfig, RubricLevel, SemanticMatchConfig};
use adk_eval::schema::ToolUse;
use adk_eval::{
    AutoraterConfig, PointwiseMetricSpec, TrajectoryMetric, VertexEvalClient, VertexEvalConfig,
    VertexEvalJudge,
};
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use google_cloud_auth::credentials::api_key_credentials;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::Mutex;

const PROJECT: &str = "test-project";
const LOCATION: &str = "us-central1";

#[derive(Default)]
struct MockState {
    bodies: Vec<Value>,
    /// Queue of responses, popped per request; empty falls back to `{}`.
    responses: Vec<Value>,
    /// When set, every request fails with this HTTP status.
    fail_status: Option<u16>,
}

type SharedState = Arc<Mutex<MockState>>;

async fn start_mock(state: SharedState) -> String {
    let path = format!("/v1beta1/projects/{PROJECT}/locations/{LOCATION}:evaluateInstances");
    let app = Router::new()
        .route(
            &path,
            post(|State(state): State<SharedState>, Json(body): Json<Value>| async move {
                let mut state = state.lock().await;
                state.bodies.push(body);
                if let Some(status) = state.fail_status {
                    let status = axum::http::StatusCode::from_u16(status).unwrap();
                    return (status, Json(json!({ "error": { "message": "mock failure" } })));
                }
                let response =
                    if state.responses.is_empty() { json!({}) } else { state.responses.remove(0) };
                (axum::http::StatusCode::OK, Json(response))
            }),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

fn build_client(endpoint: &str) -> VertexEvalClient {
    let config = VertexEvalConfig::new(PROJECT, LOCATION).with_endpoint(endpoint);
    let credentials = api_key_credentials::Builder::new("test-api-key").build();
    VertexEvalClient::with_credentials(config, credentials).expect("build test client")
}

fn pointwise_response(score: f64, explanation: &str) -> Value {
    json!({ "pointwiseMetricResult": { "score": score, "explanation": explanation } })
}

#[tokio::test]
async fn semantic_match_sends_pointwise_input_and_maps_the_result() {
    let state = SharedState::default();
    state.lock().await.responses.push(pointwise_response(0.9, "Same meaning."));
    let endpoint = start_mock(state.clone()).await;
    let judge = VertexEvalJudge::new(build_client(&endpoint));

    let custom = SemanticMatchConfig {
        judge_model: "unused".to_string(),
        custom_prompt: Some("Compare {expected} with {actual}. Score 0.0-1.0.".to_string()),
    };
    let result =
        judge.semantic_match("It is sunny", "The weather is sunny", Some(&custom)).await.unwrap();

    let captured = state.lock().await;
    assert_eq!(captured.bodies.len(), 1);
    // Whole-value comparison: this is the exact wire body the service and
    // python-aiplatform share — jsonInstance is a JSON-encoded string.
    assert_eq!(
        captured.bodies[0],
        json!({
            "pointwiseMetricInput": {
                "metricSpec": {
                    "metricPromptTemplate": "Compare {expected} with {actual}. Score 0.0-1.0.",
                },
                "instance": {
                    "jsonInstance":
                        json!({ "expected": "It is sunny", "actual": "The weather is sunny" })
                            .to_string(),
                },
            }
        }),
    );

    assert!((result.score - 0.9).abs() < f64::EPSILON);
    assert!(result.equivalent);
    assert_eq!(result.reasoning, "Same meaning.");
}

#[tokio::test]
async fn low_semantic_scores_are_not_equivalent() {
    let state = SharedState::default();
    state.lock().await.responses.push(pointwise_response(0.2, "Different subjects."));
    let endpoint = start_mock(state.clone()).await;
    let judge = VertexEvalJudge::new(build_client(&endpoint));

    let result = judge.semantic_match("It is sunny", "It is raining", None).await.unwrap();
    assert!(!result.equivalent);

    let captured = state.lock().await;
    let template =
        &captured.bodies[0]["pointwiseMetricInput"]["metricSpec"]["metricPromptTemplate"];
    let template = template.as_str().unwrap();
    assert!(template.contains("{expected}"), "default template keeps placeholders: {template}");
    assert!(template.contains("{actual}"), "default template keeps placeholders: {template}");
}

#[tokio::test]
async fn rubric_evaluation_sends_one_call_per_rubric_and_aggregates_by_weight() {
    let state = SharedState::default();
    {
        let mut lock = state.lock().await;
        lock.responses.push(pointwise_response(1.0, "Accurate."));
        lock.responses.push(pointwise_response(0.5, "Somewhat clear."));
    }
    let endpoint = start_mock(state.clone()).await;
    let judge = VertexEvalJudge::new(build_client(&endpoint));

    let config = RubricConfig {
        rubrics: vec![
            Rubric::new("Accuracy", "Response is factually correct").with_weight(3.0).with_levels(
                vec![RubricLevel { score: 1.0, description: "Completely accurate".to_string() }],
            ),
            Rubric::new("Clarity", "Response is easy to follow").with_weight(1.0),
        ],
    };
    let result =
        judge.evaluate_rubrics("The answer is 42.", "Deep Thought", &config).await.unwrap();

    let captured = state.lock().await;
    assert_eq!(captured.bodies.len(), 2);
    let first_template =
        captured.bodies[0]["pointwiseMetricInput"]["metricSpec"]["metricPromptTemplate"]
            .as_str()
            .unwrap();
    assert!(first_template.contains("Rubric: Accuracy"), "{first_template}");
    assert!(first_template.contains("- 1.0: Completely accurate"), "{first_template}");
    assert_eq!(
        captured.bodies[0]["pointwiseMetricInput"]["instance"],
        json!({
            "jsonInstance":
                json!({ "response": "The answer is 42.", "context": "Deep Thought" }).to_string(),
        }),
    );

    // (1.0 * 3 + 0.5 * 1) / 4 = 0.875
    assert!((result.overall_score - 0.875).abs() < f64::EPSILON);
    assert_eq!(result.rubric_scores.len(), 2);
    assert_eq!(result.rubric_scores[0].name, "Accuracy");
    assert_eq!(result.rubric_scores[1].reasoning, "Somewhat clear.");
}

#[tokio::test]
async fn safety_and_hallucination_verdicts_fold_the_explanation_into_issues() {
    let state = SharedState::default();
    {
        let mut lock = state.lock().await;
        lock.responses.push(pointwise_response(0.1, "Encourages illegal activity."));
        lock.responses.push(pointwise_response(1.0, "Grounded in the context."));
    }
    let endpoint = start_mock(state.clone()).await;
    let judge = VertexEvalJudge::new(build_client(&endpoint));

    let safety = judge.evaluate_safety("bad response").await.unwrap();
    assert!(!safety.is_safe);
    assert_eq!(safety.issues, vec!["Encourages illegal activity.".to_string()]);

    let hallucination = judge
        .detect_hallucinations("It rains in Hamburg.", "Hamburg weather report", Some("It rains."))
        .await
        .unwrap();
    assert!(hallucination.hallucination_free);
    assert!(hallucination.issues.is_empty());

    let captured = state.lock().await;
    let instance =
        captured.bodies[1]["pointwiseMetricInput"]["instance"]["jsonInstance"].as_str().unwrap();
    let instance: Value = serde_json::from_str(instance).unwrap();
    assert_eq!(instance["ground_truth"], "It rains.");
    let template = captured.bodies[1]["pointwiseMetricInput"]["metricSpec"]["metricPromptTemplate"]
        .as_str()
        .unwrap();
    assert!(template.contains("{ground_truth}"), "{template}");
}

#[tokio::test]
async fn trajectory_exact_match_sends_tool_calls_and_parses_the_score() {
    let state = SharedState::default();
    state.lock().await.responses.push(json!({
        "trajectoryExactMatchResults": {
            "trajectoryExactMatchMetricValues": [{ "score": 1.0 }],
        }
    }));
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let predicted = vec![
        ToolUse::new("get_weather").with_args(json!({ "city": "Paris" })),
        ToolUse { name: "no_args".to_string(), args: Value::Null, expected_response: None },
    ];
    let reference = vec![ToolUse::new("get_weather").with_args(json!({ "city": "Paris" }))];
    let score = client
        .evaluate_trajectory(TrajectoryMetric::ExactMatch, &predicted, &reference)
        .await
        .unwrap();
    assert_eq!(score, 1.0);

    let captured = state.lock().await;
    assert_eq!(
        captured.bodies[0],
        json!({
            "trajectoryExactMatchInput": {
                "metricSpec": {},
                "instances": [{
                    "predictedTrajectory": {
                        "toolCalls": [
                            {
                                "toolName": "get_weather",
                                "toolInput": json!({ "city": "Paris" }).to_string(),
                            },
                            { "toolName": "no_args" },
                        ]
                    },
                    "referenceTrajectory": {
                        "toolCalls": [
                            {
                                "toolName": "get_weather",
                                "toolInput": json!({ "city": "Paris" }).to_string(),
                            },
                        ]
                    },
                }],
            }
        }),
    );
}

#[tokio::test]
async fn trajectory_recall_reads_its_own_result_key() {
    let state = SharedState::default();
    state.lock().await.responses.push(json!({
        "trajectoryRecallResults": {
            "trajectoryRecallMetricValues": [{ "score": 0.5 }],
        }
    }));
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let tool_uses = vec![ToolUse::new("a")];
    let score =
        client.evaluate_trajectory(TrajectoryMetric::Recall, &tool_uses, &tool_uses).await.unwrap();
    assert_eq!(score, 0.5);
    assert!(
        state.lock().await.bodies[0].get("trajectoryRecallInput").is_some(),
        "recall uses its own input key",
    );
}

#[tokio::test]
async fn autorater_config_rides_along_on_pointwise_requests() {
    let state = SharedState::default();
    state.lock().await.responses.push(pointwise_response(1.0, "ok"));
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint).with_autorater_config(
        AutoraterConfig::new()
            .with_autorater_model(format!(
                "projects/{PROJECT}/locations/{LOCATION}/publishers/google/models/gemini-3.7-flash"
            ))
            .with_sampling_count(1),
    );

    let spec = PointwiseMetricSpec::new("Rate {response}.").with_system_instruction("Be strict.");
    let result = client.evaluate_pointwise(&spec, &json!({ "response": "hi" })).await.unwrap();
    assert_eq!(result.score, Some(1.0));

    let captured = state.lock().await;
    assert_eq!(
        captured.bodies[0],
        json!({
            "pointwiseMetricInput": {
                "metricSpec": {
                    "metricPromptTemplate": "Rate {response}.",
                    "systemInstruction": "Be strict.",
                },
                "instance": { "jsonInstance": json!({ "response": "hi" }).to_string() },
            },
            "autoraterConfig": {
                "autoraterModel":
                    format!("projects/{PROJECT}/locations/{LOCATION}/publishers/google/models/gemini-3.7-flash"),
                "samplingCount": 1,
            },
        }),
    );
}

#[tokio::test]
async fn missing_pointwise_result_is_an_invalid_response_error() {
    let state = SharedState::default();
    state.lock().await.responses.push(json!({ "metricResults": [] }));
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let error =
        client.evaluate_pointwise(&PointwiseMetricSpec::new("t"), &json!({})).await.unwrap_err();
    assert_eq!(error.code, "eval.vertex.invalid_response");
}

#[tokio::test]
async fn upstream_status_errors_carry_the_eval_identity() {
    let state = SharedState::default();
    state.lock().await.fail_status = Some(429);
    let endpoint = start_mock(state.clone()).await;
    let client = build_client(&endpoint);

    let error = client.evaluate_instances(json!({})).await.unwrap_err();
    assert_eq!(error.http_status_code(), 429);
    assert_eq!(error.code, "eval.vertex.rate_limited");
}

/// Live smoke test against the real service. Requires ADC plus
/// `GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION`.
///
/// ```bash
/// GOOGLE_CLOUD_PROJECT=p GOOGLE_CLOUD_LOCATION=us-central1 \
///   cargo nextest run -p adk-eval --features vertex-eval --run-ignored all live_
/// ```
#[tokio::test]
#[ignore = "requires GOOGLE_CLOUD_PROJECT / GOOGLE_CLOUD_LOCATION and ADC"]
async fn live_trajectory_exact_match_scores_identical_trajectories_as_one() {
    let config =
        VertexEvalConfig::from_env().expect("GOOGLE_CLOUD_PROJECT / GOOGLE_CLOUD_LOCATION");
    let client = VertexEvalClient::new_with_adc(config).expect("ADC client");

    let tool_uses = vec![ToolUse::new("get_weather").with_args(json!({ "city": "Paris" }))];
    let score = client
        .evaluate_trajectory(TrajectoryMetric::ExactMatch, &tool_uses, &tool_uses)
        .await
        .expect("evaluateInstances call");
    assert_eq!(score, 1.0);
}
