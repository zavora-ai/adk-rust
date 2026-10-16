- **Malformed tool-call JSON no longer ends the run** (`adk-model`, `adk-agent`): arguments
  that do not decode to a JSON object — truncated or invalid JSON, an array, a number — made
  the OpenAI Chat, OpenAI Responses, Azure OpenAI, Anthropic, Bedrock, DeepSeek, Groq, and
  Azure AI clients fail the turn with `model.<provider>.invalid_tool_arguments`, and the unary
  DeepSeek, Groq, and Azure AI paths ran the tool with `{}` instead. Each client now emits the
  call with `adk_core::invalid_tool_arguments(reason, raw)` arguments, an Anthropic call cut
  off by `max_tokens` included, and `LlmAgent` answers it with an error without executing the
  tool, so the model can send the call again.
