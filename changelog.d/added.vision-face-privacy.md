- **Faces, not whole people, and blur on streams** (`adk-vision`, `adk-harness`, `adk-cli`):
  the YuNet face detector (MIT, `yunet-2023mar`, 232 KB) joins the built-in manifest as
  `onnx::YuNet`. `FaceRedactor` covers each face it finds and, with RF-DETR Nano as its
  person fallback, each person it finds no face for as the whole person; it is the default
  `faces` redactor of the harness vision pack and of `adk-rust vision run`, so a close desk
  frame is no longer blanked by one person's box. Approval cards draw whole persons as
  `people` and their captions say `1 face covered` or `1 face · the whole person is
  covered`. `RedactorPrivacy::blur_faces` blurs faces on a frame stream before any write,
  with `RedactionMethod::ScaledBlur` and a two-frame hold (`RedactorPrivacy::with_hold`), and `stream::fixtures::draw_portrait` draws a
  generated face for tests (spec 3.7 Q3, S18).
- **The on-device profile** (`adk-vision`, `adk-harness`, `adk-cli`): `on_device = true` in
  the `[egress]` table of `.adk/vision.toml` refuses every image release at the egress
  gate's destination step with its reason, and the default model fetchers work offline. An
  escalation the gate refuses now leaves the on-device answer standing instead of failing
  the call (spec 3.7 S19).
