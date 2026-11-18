- **GenAI spans follow the current OpenTelemetry GenAI conventions** (`adk-telemetry`,
  `adk-model`, `adk-agent`, `adk-runner`): the conventions of the `semantic-conventions-genai`
  repository (`main` as of 2026-11-18, schema `gen-ai-dev/1.42.0-dev`; the conventions left the
  core repository in v1.42.0). Every provider's model call exports as `{operation} {model}`
  (`chat claude-sonnet-5-5`, `generate_content gemini-3.7-flash`), kind client, with
  `gen_ai.operation.name`, `gen_ai.provider.name` (`gcp.gemini`, `gcp.vertex_ai`,
  `aws.bedrock`, `azure.ai.inference`, `mistral`, …), the request parameters, the response
  model and finish reasons (`stop`, `length`, `content_filter`, `tool_call`, `error`) and
  `error.type`; the runner's agent span exports as `invoke_agent {agent}` and `LlmAgent`'s tool
  span as `execute_tool {tool}` with `gen_ai.tool.*` attributes. `gen_ai.system`,
  `gen_ai.usage.total_tokens`, `cache_read_tokens`, `cache_creation_tokens`, `thinking_tokens`
  and `audio_*_tokens` are replaced by `gen_ai.provider.name`, `gen_ai.usage.cache_read.input_tokens`,
  `cache_write.input_tokens`, `reasoning.output_tokens` and `audio.{input,output}_tokens`, with
  no total. Conversation capture is opt-in through `OTEL_INSTRUMENTATION_GENAI_CAPTURE_CONTENT`
  or `config::set_capture_content` (`gen_ai.input.messages`, `gen_ai.output.messages`,
  `gen_ai.system_instructions`, `gen_ai.tool.definitions`, `gen_ai.tool.call.arguments` and
  `.result`, or the `gen_ai.client.inference.operation.details` event).
  `SemconvConfig::capture_content` is a `CaptureContent`, `ContentEventEmitter` emits the
  details event instead of `gen_ai.content.prompt` and `gen_ai.content.completion`, the retired
  attribute constants are gone, `GenAiSpanBuilder::top_k` takes an integer, and
  `GenAiProvider` and `GenAiOperation` are `#[non_exhaustive]`.
