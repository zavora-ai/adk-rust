- **Governed vision tools and the image privacy guardrail** (`adk-vision`): `VisionToolset`
  offers `vision_detect`, `vision_segment`, `vision_read`, `vision_describe`, `vision_crop`,
  `vision_redact` and `vision_annotate` on the governed tool path. A call bound for a remote
  backend is held with the exact redacted image it would send (`EgressGate::stage`, the copy
  stored as an artifact), its egress policy applies `.adk/vision.toml` per call, and approval
  releases only that digest. Budget holds open before any image is released, token-billed
  backends are estimated with `TokenEstimate`, and every call, release and edit writes a
  `vision.*` audit record. Feature `guardrail` adds `ImagePrivacyGuardrail` on model
  requests, tool results and user input, and `PersonalTextRedactor`; `PersonRedactor` covers
  each detected person's whole box.
