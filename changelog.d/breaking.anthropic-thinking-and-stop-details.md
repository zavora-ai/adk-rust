- **New Anthropic enum variants and message fields** (`adk-anthropic`, `adk-model`):
  `adk_anthropic::ThinkingConfig` gains `BetweenTools` and `adk_model::anthropic::ThinkingMode`
  gains `BetweenTools`; exhaustive matches add an arm. `adk_anthropic::Message` and
  `MessageDelta` gain `stop_details: Option<RefusalDetails>`; struct literals add the field or use
  `Message::new` / `MessageDelta::new`.
