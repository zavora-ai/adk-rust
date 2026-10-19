- **Reasoning in the final stream chunk is kept** (`adk-model`): the Chat Completions stream
  behind `OpenAIClient`, `OpenAICompatible` and its presets, `AzureOpenAIClient`, and OpenCode
  Chat dropped `reasoning_content` or `reasoning` sent in the chunk that carries
  `finish_reason`. It is now emitted as a `Part::Thinking` delta before the final text or
  tool calls.
