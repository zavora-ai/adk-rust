//! `VisionEvalAgent` (spec 3.7 §7.16): a runtime task answers the committed vision eval
//! sets in process and over the `cargo adk eval` protocol, and `adk-eval`'s vision criteria
//! score it from the tool responses it reports.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use adk_eval::vision::VisionCriteria;
use adk_eval::{EvaluationConfig, EvaluationCriteria, Evaluator, TestFile};
use adk_vision::eval::{EvalTask, VisionEvalAgent, serve_eval_protocol};
use adk_vision::{
    BackendDescriptor, CoordFrame, DecodedImage, Digest, Locality, Mask, RawRegion, SegmentPrompt,
    Segmenter, SentImage, Shape, SourceImage, VisionCx, VisionRuntime,
};
use serde_json::{Value, json};

fn eval_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/eval")
}

/// Segments every inked (non-white) pixel inside each box prompt: on a white canvas, the
/// drawn shapes.
struct InkSegmenter(BackendDescriptor);

#[async_trait::async_trait]
impl Segmenter for InkSegmenter {
    fn descriptor(&self) -> &BackendDescriptor {
        &self.0
    }

    async fn segment(
        &self,
        _cx: &VisionCx,
        image: &SentImage,
        prompts: &[SegmentPrompt],
    ) -> adk_vision::Result<Vec<RawRegion>> {
        let (w, h) = image.size();
        let pixels = image.pixels();
        Ok(prompts
            .iter()
            .filter_map(|prompt| match prompt {
                SegmentPrompt::Box { bbox } => Some(bbox),
                _ => None,
            })
            .map(|bbox| {
                let mut bits = vec![false; (w * h) as usize];
                for y in 0..h {
                    for x in 0..w {
                        let [r, g, b, _] = pixels.pixel(x, y);
                        let ink = u32::from(r) + u32::from(g) + u32::from(b) < 600;
                        bits[(y * w + x) as usize] =
                            ink && bbox.contains([x as f32 + 0.5, y as f32 + 0.5]);
                    }
                }
                RawRegion::shape(Shape::Mask(Mask::encode(w, h, &bits)))
            })
            .collect())
    }
}

fn agent() -> VisionEvalAgent {
    let descriptor = BackendDescriptor::new("ink", Locality::InProcess, CoordFrame::SentPixels);
    let runtime =
        VisionRuntime::builder().segmenter(Arc::new(InkSegmenter(descriptor))).build().unwrap();
    VisionEvalAgent::new(Arc::new(runtime), EvalTask::Segment).with_name("ink")
}

#[tokio::test]
async fn the_committed_segmentation_set_scores_through_adk_eval() {
    let test_file = TestFile::load(eval_dir().join("shapes-segment.test.json")).unwrap();
    let criteria = EvaluationCriteria::default().with_vision(VisionCriteria {
        mask_iou: Some(0.95),
        region_iou: Some(0.95),
        count_accuracy: Some(1.0),
        ..VisionCriteria::default()
    });
    let evaluator = Evaluator::new(EvaluationConfig::with_criteria(criteria));
    let report = evaluator.evaluate_test_file(Arc::new(agent()), &test_file).await.unwrap();
    let result = &report.results[0];
    assert!(result.passed, "{:?} {:?}", result.scores, result.failures);
    assert_eq!(result.scores["count_accuracy"], 1.0);
}

#[tokio::test]
async fn the_protocol_answers_verified_images_and_refuses_changed_ones() {
    let image = eval_dir().join("../cloud/shapes-960x540.png");
    let digest = Digest::of(&std::fs::read(&image).unwrap()).to_string();
    let prompts = json!([{"kind": "box", "bbox": {"x0": 74, "y0": 54, "x1": 326, "y1": 226}}]);
    let request = |digest: &str| {
        json!({
            "case_id": "shapes", "turn": 0, "session_id": "s",
            "user_text": prompts.to_string(),
            "images": [{"path": image, "digest": digest, "mime_type": "image/png"}]
        })
        .to_string()
    };
    let wrong = format!("sha256:{}", "0".repeat(64));
    let input = format!("{}\n\n{}\n{{\"user_text\": \"x\"}}\n", request(&digest), request(&wrong));
    let mut output = Vec::new();
    serve_eval_protocol(&agent(), input.as_bytes(), &mut output).await.unwrap();

    let lines: Vec<Value> = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    let call = &lines[0]["tool_calls"][0];
    assert_eq!(call["name"], "vision_segment");
    assert_eq!(call["args"]["prompts"][0]["kind"], "box");
    let region = &call["response"]["regions"][0];
    assert_eq!(region["mask"]["width"], 960);
    assert_eq!(call["response"]["calls"][0]["backend"], "ink");
    assert_eq!(call["response"]["calls"][0]["remote"], false);
    assert!(lines[1]["error"].as_str().unwrap().contains("not the sha256:0000"), "{}", lines[1]);
    assert!(lines[2]["error"].as_str().unwrap().contains("shows no image"), "{}", lines[2]);
}

#[tokio::test]
async fn a_turn_with_two_images_is_refused_by_single_image_tasks() {
    let image = SourceImage::from_pixels(DecodedImage::filled(4, 4, [255; 4])).unwrap();
    let err = agent().answer(&VisionCx::new(), &[image.clone(), image], "[]").await.unwrap_err();
    assert!(err.to_string().contains("one image per turn"), "{err}");
}
