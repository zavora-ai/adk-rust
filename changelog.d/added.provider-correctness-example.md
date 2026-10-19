- **`provider_correctness` example** (`examples/provider_correctness`): drives `LlmAgent` and
  `Runner` with the OpenAI Chat and Responses, Azure OpenAI, Anthropic, Groq, DeepSeek, Azure AI,
  and Ollama clients against the live APIs and a scripted local upstream, and checks the requests
  they send: tool messages, structured output, generation config fields, tool images, idle
  timeouts, and DeepSeek-V3 native tool calls.
- **`stream_termination` example** (`examples/stream_termination`): runs the Chat Completions,
  Groq, Ollama, and OpenRouter clients through `LlmAgent` and `Runner` against a local upstream
  that cuts a stream mid-answer or mid-tool-call, sends an error frame, or splits a UTF-8
  character, and checks that the run fails or succeeds as it should in both streaming modes.
- **`model_followups` example** (`examples/model_followups`): streamed Ollama usage with a missing
  count, DeepSeek and Azure AI stream error frames, and Chat Completions refusals, through
  `LlmAgent` and `Runner`.
