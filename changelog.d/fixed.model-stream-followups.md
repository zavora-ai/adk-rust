- **Chat Completions refusals reach the agent** (`adk-model`): a `refusal` with
  `content: null`, streamed or not, became an empty turn finishing with `Stop` on
  `OpenAIClient`, `OpenAICompatible` and its presets, `AzureOpenAIClient`, and OpenCode
  Chat. The refusal text is now the response's content and the turn finishes with
  `FinishReason::Safety`.
