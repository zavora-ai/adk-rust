- **Stream and camera privacy hooks are opt-in** (`adk-vision`, `adk-surface`):
  `StreamPipeline::builder`, `SourceSpec::new`, `LiveCamera::stream` and
  `LiveCamera::surface` no longer take a `PrivacyHook`; frames pass unredacted (`NoRedaction`)
  unless `with_privacy(hook)` gives one. `VideoAnalystBuilder::keyframes` no longer takes a
  `FramePrivacy`; `keyframe_privacy(hook)` opts in. The live camera covers detected people
  only under `LiveCamera::cover_people`.
