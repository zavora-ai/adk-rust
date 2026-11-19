- **Vision evals** (`adk-eval`, `adk-vision`, `cargo-adk`; spec 3.7 §7.16, X13): an eval case
  shows the agent image fixtures named by path and pinned by SHA-256, verified when the eval
  file loads (`ImageFixture`). A turn's `expected_vision` states the regions, masks, page text,
  fields, click targets and count the agent must perceive, and `EvaluationCriteria::vision`
  scores them in source pixels — region IoU and box edge error, mask IoU, OCR character and
  word accuracy, exact and normalised field match, detection precision, recall, AP and COCO
  mAP, grounding hit rate and count — from the agent's vision tool responses. `cargo adk eval`
  passes fixtures to the agent process by path and digest and reads vision answers from each
  tool call's `response`, so baselines and `--check-regression` gate vision backends.
  `adk-vision`'s `eval` feature adds `VisionEvalAgent` and `serve_eval_protocol`, which answer
  eval cases with any runtime task. Six eval sets over generated fixtures ship in
  `adk-vision/tests/fixtures/eval/`.
