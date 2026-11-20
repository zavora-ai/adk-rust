- **`VoiceConfig` gains `governance` and `tools`** (`adk-harness`): a struct literal of
  `adk_harness::VoiceConfig` needs both fields (or `VoiceConfig::new`), and
  `adk_surface::voice::VoiceModels` needs `agent_name`. `VoiceConfig` no longer derives
  `Debug` through its fields; its `Debug` lists the tools by name.
