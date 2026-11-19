# Vision Evals

`adk-eval` scores what an agent perceives: the regions it finds, the masks it draws, the text
it reads, the fields it extracts, the detections it reports and the widgets it would click.
Eval cases show the agent image fixtures pinned by SHA-256, every score is computed in the
source image's pixels, and the scores gate a change through thresholds and saved baselines,
in a Rust test or through `cargo adk eval`.

> **Note:** `adk-vision` is **Experimental** (spec 3.7, §7.16). The scorers live in
> `adk-eval` and depend on no vision crate; `adk-vision`'s `eval` feature turns any
> `VisionRuntime` task into an eval agent.

## Image Fixtures

An `image` part names a file relative to the eval file and pins its digest. `TestFile::load`
and `EvalSet::load` read every fixture and refuse the file when one is missing, larger than
10 MiB, or not the file its digest pins, so a case never runs against an image that changed
after its expectations were written. The agent receives the verified bytes as
`Part::InlineData`.

```json
{
  "invocation_id": "t0",
  "user_content": {
    "role": "user",
    "parts": [
      {"image": {"path": "scene-1024x640.png", "digest": "sha256:c0e126b352e65420f0072d06ee1f15b73d3fc90e930adcd44a525954999048a9"}},
      {"text": "stop sign, clock"}
    ]
  },
  "expected_vision": {
    "regions": [
      {"label": "stop sign", "box": [121.4, 111.4, 398.6, 388.6]},
      {"label": "clock", "box": [610, 100, 871, 361]}
    ],
    "count": 2
  }
}
```

`ImageFixture::digest_of` gives a file's digest in the pinned form. A case built in code
calls `EvalCase::load_images` before it runs; the evaluator refuses a case whose images
were never verified.

## Expected Perception

`expected_vision` on a turn states what the agent must perceive. Each part present is scored;
parts left out are not. Boxes are `[x0, y0, x1, y1]` in source pixels with `x1` and `y1`
exclusive, and masks are COCO run-length encodings (`width`, `height`, `counts`, column-major,
unset run first) — the wire form `adk-vision` publishes in `schema/region-set.json`.

| Part | Holds | Scores |
|------|-------|--------|
| `regions` | `label`, `box`, optional `mask` | `region_iou`, `box_hit_rate`, `mask_iou`, `detection_precision`, `detection_recall`, `detection_ap`, `detection_map` |
| `text` | The page's text in reading order | `ocr_char_accuracy`, `ocr_word_accuracy` |
| `fields` | Field name to value | `field_exact_match`, `field_normalized_match` |
| `targets` | `label` and `box` of each click target | `grounding_hit_rate` |
| `count` | How many regions the answer holds | `count_accuracy` |

## Scores

Every score runs from 0 to 1, higher is better, so a baseline catches a drop in any of them.

| Score | Definition |
|-------|------------|
| `region_iou` | Mean box IoU of each expected region with its answer region; regions pair one to one, best IoU first, labels matching |
| `box_hit_rate` | Share of expected regions whose paired box has every edge within `box_tolerance_px` (default 2) |
| `mask_iou` | Mean mask IoU of each expected mask with its paired answer mask |
| `ocr_char_accuracy` | `1 − CER`: character edit distance over the expected text's length, whitespace collapsed |
| `ocr_word_accuracy` | `1 − WER`: word edit distance over the expected word count |
| `field_exact_match` | Share of expected fields whose value is identical after trimming |
| `field_normalized_match` | The same ignoring case, spacing and punctuation, with numbers compared by value (`$58.2` matches `58.20`) |
| `detection_precision`, `detection_recall` | COCO's greedy matching, highest score first, at `iou_threshold` (default 0.5) |
| `detection_ap` | COCO's 101-point interpolated average precision at `iou_threshold`, averaged over the expected labels |
| `detection_map` | `detection_ap` averaged over IoU thresholds 0.50, 0.55, …, 0.95 |
| `grounding_hit_rate` | Share of targets whose matching answer region's centre — where a click lands — falls inside the target |
| `count_accuracy` | `1 − |found − expected| / expected` |

Labels match when they are equal ignoring case and punctuation, or one contains the other; an
expected region without a label matches any answer region. A missing answer part scores 0.
Free-text answers — what kind of document this is, whether a seal is torn — use the existing
`semantic_match_score`, `rubric_quality_score` and LLM judge on the turn's `final_response`.

## Where Answers Come From

The evaluator reads the agent's answer from the responses of the vision tools it called, in
the form those tools return them:

| Tool | Gives |
|------|-------|
| `vision_detect`, `vision_segment` | `regions` (`box`, `label`, `score`, and `mask` when present) |
| `vision_read` | `blocks`, joined in order as the page's text, and `fields` when present |
| `vision_describe` | `text`, `value` (its top-level scalars become fields) and `regions` |

A later response replaces the parts an earlier one gave. When no tool gave fields, a final
response that is a JSON object supplies them, so an agent that extracts fields as structured
output is scored the same way.

## Criteria

`EvaluationCriteria::vision` holds a threshold per score; a score without a threshold is not
computed. In a criteria file:

```json
{
  "vision": {
    "iou_threshold": 0.5,
    "box_tolerance_px": 2.0,
    "region_iou": 0.75,
    "detection_recall": 1.0,
    "ocr_char_accuracy": 0.95,
    "field_normalized_match": 0.85
  }
}
```

A score below its threshold fails the case with the score, the largest box edge error, and
what missed: the region no answer matched, the field that differs, the target a click missed.

## A Backend as an Eval Agent

`adk-vision`'s `eval` feature provides `VisionEvalAgent`: one `VisionRuntime` task answering
every turn and reporting the answer as the matching vision tool's call and response, so a bare
backend is scored exactly as an agent that called the tool.

| `EvalTask` | Tool | The turn's text is |
|------------|------|--------------------|
| `Detect { concepts }` | `vision_detect` | Comma-separated classes (empty: every class), or open-vocabulary concepts |
| `Segment` | `vision_segment` | A JSON array of `SegmentPrompt`s, else one concept |
| `Read { markdown, fields }` | `vision_read` | Ignored; with `fields`, `Label: value` lines become fields |
| `Describe { want_regions, schema }` | `vision_describe` | The question |

```rust
use std::sync::Arc;

use adk_eval::vision::VisionCriteria;
use adk_eval::{EvaluationConfig, EvaluationCriteria, Evaluator, TestFile};
use adk_vision::eval::{EvalTask, VisionEvalAgent};
use adk_vision::{FetchConfig, InMemoryAcceptanceStore, ModelFetcher, PpOcrReader, PpOcrV6Size, VisionRuntime};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let fetcher =
        ModelFetcher::new(FetchConfig::from_env(), Arc::new(InMemoryAcceptanceStore::new()));
    let reader = PpOcrReader::fetch(&fetcher, PpOcrV6Size::Tiny).await?;
    let runtime = VisionRuntime::builder().reader(Arc::new(reader)).build()?;
    let agent = VisionEvalAgent::new(Arc::new(runtime), EvalTask::Read { markdown: false, fields: true });

    let criteria = EvaluationCriteria::default().with_vision(VisionCriteria {
        ocr_char_accuracy: Some(0.95),
        field_normalized_match: Some(0.85),
        ..VisionCriteria::default()
    });
    let evaluator = Evaluator::new(EvaluationConfig::with_criteria(criteria));
    let test_file = TestFile::load("adk-vision/tests/fixtures/eval/documents-read.test.json")?;
    let report = evaluator.evaluate_test_file(Arc::new(agent), &test_file).await?;
    assert!(report.all_passed(), "{}", report.format_summary());
    Ok(())
}
```

```toml
[dependencies]
adk-eval = "3.0.0"
adk-vision = { version = "3.0.0", features = ["eval", "ocr"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## With `cargo adk eval`

`cargo adk eval` passes each turn's fixtures to the agent process by path and digest, and
reads vision answers from the `response` of each tool call it writes back:

```text
→ {"case_id":"invoice","turn":0,"user_text":"Read this page.","session_id":"…",
   "images":[{"path":"/…/invoice-personal-1000x1300.png","digest":"sha256:…","mime_type":"image/png"}]}
← {"tool_calls":[{"name":"vision_read","args":{"markdown":false},"response":{"blocks":[…],"fields":{…}}}]}
```

`adk_vision::eval::serve_eval_protocol` answers that protocol for a `VisionEvalAgent`,
checking each image against its digest before it decodes it. The `vision_bench` example in
`adk-bench` serves any built-in local backend by name:

```bash
cargo build -p adk-bench --profile ci --features vision-onnx,vision-ocr --example vision_bench

# Score PP-OCRv6 tiny and save its baseline
cargo adk eval adk-vision/tests/fixtures/eval/documents-read.test.json \
    --criteria adk-vision/tests/fixtures/eval/criteria/read.json \
    --agent-cmd "target/ci/examples/vision_bench serve pp-ocrv6-tiny" \
    --baseline adk-vision/tests/fixtures/eval/baselines/pp-ocrv6-tiny.documents-read.json \
    --save-baseline

# A later change fails when any score drops more than the tolerance
cargo adk eval adk-vision/tests/fixtures/eval/documents-read.test.json \
    --criteria adk-vision/tests/fixtures/eval/criteria/read.json \
    --agent-cmd "target/ci/examples/vision_bench serve pp-ocrv6-tiny" \
    --baseline adk-vision/tests/fixtures/eval/baselines/pp-ocrv6-tiny.documents-read.json \
    --check-regression --tolerance 0.02
```

A missing baseline fails `--check-regression` rather than passing it.

## The Committed Eval Sets

`adk-vision/tests/fixtures/eval/` holds eval sets over images the project generates; each is
written by `generate.py` from the fixtures' ground truth, and `scene.py` draws the detection
scene.

| Eval set | Image | Expects | For |
|----------|-------|---------|-----|
| `generated-detect` | A drawn stop sign and wall clock, 1024×640 | Two labelled boxes, count | Closed-set detectors (RF-DETR) |
| `shapes-segment` | Three drawn shapes, 960×540; box prompts 6 px outside each | Exact masks, boxes, count | Promptable segmenters (EfficientSAM, SAM 2.1) |
| `shapes-concepts` | The same shapes, one named concept per case | Labelled box and mask | Open-vocabulary segmenters (`sam-3.1`) |
| `shapes-locate` | The same shapes and two text lines | Five labelled boxes | Describers asked for regions |
| `s5-grounding` | The S5 screenshot, 3024×1964 at device pixel ratio 2 | Twelve click targets | Grounding |
| `documents-read` | A report page and an invoice, 1000×1300 | Page text in reading order; the invoice's seven `Label: value` fields | Document readers |

`criteria/` holds a criteria file per kind, and `baselines/` the scores each built-in local
backend recorded when it was accepted; the nightly vision job gates every backend against its
baseline. Latency and the published results are on [Vision Benchmarks](benchmarks.md).

## Related

- [Vision Benchmarks](benchmarks.md)
- [Vision overview](index.md)
- [Local ONNX backends](onnx.md)
- [Document readers](document-readers.md)
- [Cloud vision backends](cloud.md)
