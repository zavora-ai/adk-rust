- **Ollama honours the output cap and reports why a turn ended** (`adk-model`):
  `OllamaModel` left `max_output_tokens` out of the request, so generation ran uncapped, and it
  finished every complete response with `FinishReason::Stop`. `max_output_tokens` is now sent as
  `options.num_predict`, and `done_reason` maps `stop` to `Stop`, `length` to `MaxTokens`, and
  anything else to `Other`.
