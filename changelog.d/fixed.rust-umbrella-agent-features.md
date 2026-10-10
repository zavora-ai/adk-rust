- **`guardrail`, `plugin`, and `skills` reach `LlmAgent`** (`adk-rust`): the umbrella
  features turn on the matching `adk-agent` features (`guardrails`, `enhanced-plugins`,
  `skills`). Without them, `LlmAgentBuilder::input_guardrails`, `output_guardrails`, and
  `tool_guardrails` took placeholder sets that never ran, even under `full`;
  `enhanced_plugin` did not exist; and the skill methods existed only when another crate in
  the build enabled `adk-agent/skills`.
