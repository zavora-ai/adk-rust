//! Vision eval cases (spec 3.7 §7.16, X13): image fixtures pinned by digest and verified on
//! load, shown to the agent as inline data, and vision scores read from its tool responses,
//! gated by thresholds and by a saved baseline.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use adk_core::{Agent, Content, Event, EventStream, FunctionResponseData, InvocationContext, Part};
use adk_eval::vision::VisionCriteria;
use adk_eval::{
    BaselineStore, EvalError, EvaluationConfig, EvaluationCriteria, Evaluator, ImageFixture,
    TestFile,
};
use async_trait::async_trait;
use serde_json::{Value, json};

/// What the agent was shown: the inline image's digest and its fixture annotation.
#[derive(Debug, Clone, PartialEq)]
struct Seen {
    digest: String,
    mime_type: String,
    annotation: Value,
}

/// Answers every turn with a `vision_detect` and a `vision_read` response, its boxes moved
/// `shift` pixels right, and records the images it was shown.
struct SightAgent {
    shift: f64,
    text: &'static str,
    seen: Mutex<Vec<Seen>>,
}

impl SightAgent {
    fn new(shift: f64, text: &'static str) -> Arc<Self> {
        Arc::new(Self { shift, text, seen: Mutex::new(Vec::new()) })
    }
}

#[async_trait]
impl Agent for SightAgent {
    fn name(&self) -> &str {
        "sight"
    }

    fn description(&self) -> &str {
        "answers with fixed regions"
    }

    fn sub_agents(&self) -> &[Arc<dyn Agent>] {
        &[]
    }

    async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
        for part in &ctx.user_content().parts {
            if let Part::InlineData { mime_type, data, annotations, .. } = part {
                self.seen.lock().unwrap().push(Seen {
                    digest: ImageFixture::digest_of(data),
                    mime_type: mime_type.clone(),
                    annotation: annotations.clone().unwrap_or_default(),
                });
            }
        }
        let s = self.shift;
        let responses = [
            (
                "vision_detect",
                json!({"regions": [
                    {"box": [80.0 + s, 60.0, 320.0 + s, 220.0], "label": "red rectangle", "score": 0.9},
                    {"box": [550.0 + s, 60.0, 730.0 + s, 240.0], "label": "blue circle", "score": 0.8}
                ]}),
            ),
            ("vision_read", json!({"blocks": [{"kind": "text", "text": self.text}]})),
        ];
        let mut event = Event::new(ctx.invocation_id());
        event.author = self.name().to_string();
        event.llm_response.content = Some(Content {
            role: "function".to_string(),
            parts: responses
                .into_iter()
                .map(|(name, response)| Part::FunctionResponse {
                    function_response: FunctionResponseData::new(name, response),
                    id: None,
                    annotations: None,
                })
                .collect(),
        });
        Ok(Box::pin(futures::stream::iter([Ok(event)])))
    }
}

const IMAGE: &[u8] = b"\x89PNG\r\n\x1a\n stands in for the shapes fixture";

/// Writes `img/shapes.png` and `shapes.test.json` pinning it, into `dir`.
fn write_eval_set(dir: &Path, digest: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir.join("img")).unwrap();
    std::fs::write(dir.join("img/shapes.png"), IMAGE).unwrap();
    let file = json!({
        "eval_set_id": "shapes",
        "name": "Shapes",
        "eval_cases": [{
            "eval_id": "find_shapes",
            "conversation": [{
                "invocation_id": "t0",
                "user_content": {"role": "user", "parts": [
                    {"image": {"path": "img/shapes.png", "digest": digest}},
                    {"text": "Find the red rectangle and the blue circle, and read the caption."}
                ]},
                "expected_vision": {
                    "regions": [
                        {"label": "red rectangle", "box": [80, 60, 320, 220]},
                        {"label": "blue circle", "box": [550, 60, 730, 240]}
                    ],
                    "text": "INVOICE 4721"
                }
            }]
        }]
    });
    let path = dir.join("shapes.test.json");
    std::fs::write(&path, serde_json::to_string_pretty(&file).unwrap()).unwrap();
    path
}

fn criteria() -> EvaluationCriteria {
    EvaluationCriteria::default().with_vision(VisionCriteria {
        region_iou: Some(0.97),
        box_hit_rate: Some(1.0),
        detection_recall: Some(1.0),
        ocr_char_accuracy: Some(0.9),
        ..VisionCriteria::default()
    })
}

#[test]
fn image_fixtures_are_verified_on_load() {
    let dir = tempfile::tempdir().unwrap();
    let digest = ImageFixture::digest_of(IMAGE);
    let path = write_eval_set(dir.path(), &digest);
    let loaded = TestFile::load(&path).unwrap();
    assert!(loaded.eval_cases[0].unloaded_images().is_empty());

    // The bare hex digits are accepted too.
    write_eval_set(dir.path(), digest.trim_start_matches("sha256:"));
    TestFile::load(&path).unwrap();

    // One changed byte is refused before any case runs.
    let mut changed = IMAGE.to_vec();
    *changed.last_mut().unwrap() = b'E';
    std::fs::write(dir.path().join("img/shapes.png"), changed).unwrap();
    let err = TestFile::load(&path).unwrap_err().to_string();
    assert!(err.contains("find_shapes") && err.contains("does not match"), "{err}");

    std::fs::remove_file(dir.path().join("img/shapes.png")).unwrap();
    let err = TestFile::load(&path).unwrap_err().to_string();
    assert!(err.contains("could not be read"), "{err}");

    write_eval_set(dir.path(), "sha256:1234");
    let err = TestFile::load(&path).unwrap_err().to_string();
    assert!(err.contains("64 hex digits"), "{err}");
}

#[tokio::test]
async fn the_agent_sees_the_verified_image_and_vision_scores_gate_the_case() {
    let dir = tempfile::tempdir().unwrap();
    let digest = ImageFixture::digest_of(IMAGE);
    let test_file = TestFile::load(write_eval_set(dir.path(), &digest)).unwrap();
    let evaluator = Evaluator::new(EvaluationConfig::with_criteria(criteria()));

    let exact = SightAgent::new(0.0, "INVOICE 4721");
    let report = evaluator.evaluate_test_file(exact.clone(), &test_file).await.unwrap();
    let result = &report.results[0];
    assert!(result.passed, "{:?}", result.failures);
    let expected_scores: HashMap<String, f64> = [
        ("region_iou", 1.0),
        ("box_hit_rate", 1.0),
        ("detection_recall", 1.0),
        ("ocr_char_accuracy", 1.0),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect();
    assert_eq!(result.scores, expected_scores);

    let resolved = dir.path().join("img/shapes.png");
    assert_eq!(
        *exact.seen.lock().unwrap(),
        vec![Seen {
            digest: digest.clone(),
            mime_type: "image/png".to_string(),
            annotation: json!({"adkEvalFixture": {
                "path": resolved.to_string_lossy(),
                "digest": digest,
            }}),
        }]
    );

    // The scores become the baseline; a backend 4 px off that misreads one character
    // regresses three of them and fails two thresholds.
    let baseline = BaselineStore::new(dir.path().join(".eval-baseline.json"));
    let metrics = |scores: &HashMap<String, f64>| -> HashMap<String, HashMap<String, f64>> {
        scores
            .iter()
            .map(|(name, value)| {
                (name.clone(), HashMap::from([("find_shapes".to_string(), *value)]))
            })
            .collect()
    };
    baseline.save("shapes", &metrics(&result.scores)).unwrap();

    let shifted = SightAgent::new(4.0, "INVOICE 4727");
    let report = evaluator.evaluate_test_file(shifted, &test_file).await.unwrap();
    let result = &report.results[0];
    assert!(!result.passed);
    // Rectangle 236×160 over 244×160; circle 176×180 over 184×180.
    let region_iou = (236.0 / 244.0 + 176.0 / 184.0) / 2.0;
    assert!((result.scores["region_iou"] - region_iou).abs() < 1e-12);
    assert_eq!(result.scores["box_hit_rate"], 0.0);
    assert_eq!(result.scores["detection_recall"], 1.0);
    assert_eq!(result.scores["ocr_char_accuracy"], 1.0 - 1.0 / 12.0);
    let failed: Vec<(&str, &str)> = result
        .failures
        .iter()
        .map(|f| (f.criterion.as_str(), f.details.as_deref().unwrap_or_default()))
        .collect();
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert_eq!(failed[0].0, "region_iou");
    assert_eq!(failed[1].0, "box_hit_rate");
    assert!(failed[1].1.contains("largest box edge error 4.0 px"), "{}", failed[1].1);

    let regressions = baseline.check_regressions(&metrics(&result.scores), 0.01).unwrap();
    let regressed: Vec<&str> = regressions.iter().map(|r| r.metric_name.as_str()).collect();
    assert_eq!(regressed, vec!["box_hit_rate", "ocr_char_accuracy", "region_iou"]);
}

#[tokio::test]
async fn a_turn_without_a_vision_answer_fails_closed() {
    struct Silent;

    #[async_trait]
    impl Agent for Silent {
        fn name(&self) -> &str {
            "silent"
        }
        fn description(&self) -> &str {
            "answers in prose"
        }
        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }
        async fn run(&self, ctx: Arc<dyn InvocationContext>) -> adk_core::Result<EventStream> {
            let mut event = Event::new(ctx.invocation_id());
            event.llm_response.content =
                Some(Content::new("model").with_text("I see a rectangle and a circle."));
            Ok(Box::pin(futures::stream::iter([Ok(event)])))
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let test_file =
        TestFile::load(write_eval_set(dir.path(), &ImageFixture::digest_of(IMAGE))).unwrap();
    let evaluator = Evaluator::new(EvaluationConfig::with_criteria(criteria()));
    let report = evaluator.evaluate_test_file(Arc::new(Silent), &test_file).await.unwrap();
    let result = &report.results[0];
    assert!(!result.passed);
    assert_eq!(result.scores["region_iou"], 0.0);
    assert_eq!(result.scores["ocr_char_accuracy"], 0.0);
    let details = result.failures[0].details.clone().unwrap_or_default();
    assert!(details.contains("no vision tool response"), "{details}");
}

#[tokio::test]
async fn an_unverified_image_is_refused_before_the_agent_runs() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_eval_set(dir.path(), &ImageFixture::digest_of(IMAGE));
    // Parsed without TestFile::load, so the fixture was never read or checked.
    let test_file: TestFile =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let agent = SightAgent::new(0.0, "INVOICE 4721");
    let evaluator = Evaluator::new(EvaluationConfig::with_criteria(criteria()));
    let err = evaluator.evaluate_case(agent.clone(), &test_file.eval_cases[0]).await.unwrap_err();
    assert!(matches!(err, EvalError::ConfigError(_)), "{err}");
    assert!(err.to_string().contains("not loaded and verified"), "{err}");
    assert!(agent.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn out_of_range_vision_thresholds_are_refused() {
    let criteria = EvaluationCriteria::default()
        .with_vision(VisionCriteria { region_iou: Some(75.0), ..VisionCriteria::default() });
    let evaluator = Evaluator::new(EvaluationConfig::with_criteria(criteria));
    let case: adk_eval::EvalCase = serde_json::from_value(json!({
        "eval_id": "c",
        "conversation": [{"invocation_id": "t", "user_content": {"parts": [{"text": "hi"}]}}]
    }))
    .unwrap();
    let err = evaluator.evaluate_case(SightAgent::new(0.0, ""), &case).await.unwrap_err();
    assert!(err.to_string().contains("region_iou is 75"), "{err}");
}
