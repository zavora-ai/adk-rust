- **Model Armor guardrails** (`adk-guardrail`, feature `model-armor`; `adk-rust` feature
  `gcp-model-armor`, also in `gemini-agent-platform`): `ModelArmor` screens user input
  (`sanitizeUserPrompt`), model output (`sanitizeModelResponse`) and tool results against a
  Google Cloud Model Armor template on its regional endpoint. A match blocks the content, or
  withholds the tool result, naming each matching filter in the guardrail failure; a Sensitive
  Data Protection de-identification replaces the text. Content Model Armor could not screen is
  blocked by default (`FailMode::Closed`); `FailMode::Open` passes it as a recorded warning.
