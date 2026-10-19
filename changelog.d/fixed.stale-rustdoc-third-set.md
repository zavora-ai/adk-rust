- **Rustdoc names current models and existing APIs** (`adk-agent`, `adk-browser`, `adk-gemini`,
  `adk-graph`, `adk-model`, `adk-realtime`, `adk-skill`): examples use current Gemini models,
  the deprecated `Model::Gemini31FlashLite` and `Model::Gemini3FlashPreview` variants say so,
  CodeAct instructions document `{key}` placeholders rather than `{state.key}`,
  `BrowserSessionPool::new` takes its session cap and the full browser profile has 44 tools,
  `SkillRegistryClient::new_with_adc` and `LlmAgentBuilder::new` replace calls that do not
  exist, and the functional quick start calls the checkpointed `__task_<name>` wrappers.
