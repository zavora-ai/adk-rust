- **Default on-device redactors** (`adk-vision`, `adk-harness`): `PersonRedactor::rf_detr`
  (feature `onnx`) covers each person RF-DETR Nano finds, and `PersonalTextRedactor::pp_ocr`
  (features `guardrail`, `ocr`) the personal text PP-OCRv6 reads; both load their verified
  weights on first use, and a load that fails refuses the release. `adk-harness` feature
  `vision-local` installs them in the vision pack for each required class no redactor covers;
  without it such a pack refuses every remote release. A labelled field's value returned as
  its own block beside the label is now covered too, and `PERSONAL_FIELD_LABELS` lists the
  default labels.
