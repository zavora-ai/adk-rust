- **Muse Responses mode with streaming** (`adk-model`): `MuseConfig::with_api(MuseApi::Responses)`
  drives Meta's `/responses` endpoint through the shared Open Responses engine, streamed or not,
  behind the `Llm` surface (feature `muse-responses`). Responses are attributed and priced as
  `muse`; `extensions["muse"]` carries `previous_response_id`, `store`, `reasoning`, `include` and
  `background`; settings Meta rejects (`background` with streaming, `top_logprobs`, reasoning
  effort `none`, `reasoning.encrypted_content` with `previous_response_id`, stop sequences,
  penalties) fail with `model.unsupported_config` before a request is sent.
  `OpenAIResponsesClient::with_provider_name` names the provider behind an Open Responses
  endpoint for spans, spend bounds, pricing and errors, and the engine sends
  `extensions["openai"]["store"]`.
