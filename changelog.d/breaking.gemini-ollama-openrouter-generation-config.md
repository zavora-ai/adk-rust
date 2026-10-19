- **Gemini, Ollama, and OpenRouter send or refuse every generation config field**
  (`adk-model`, `adk-gemini`): `GeminiModel`, `OllamaModel`, and OpenRouter's Responses mode
  left out fields of `GenerateContentConfig` without an error.
  - `GeminiModel` sends `stop_sequences`, `seed`, `frequency_penalty`, `presence_penalty`, and
    `top_logprobs` (as `responseLogprobs` and `logprobs`) in `generationConfig`, and returns
    Gemini's `logprobsResult` in `provider_metadata["logprobs"]`. Gemini answers penalties and
    log probabilities for a model that does not enable them, such as `gemini-3.5-flash-lite`,
    with `INVALID_ARGUMENT`, so a request that set them now fails with
    `ErrorCategory::InvalidInput` instead of running without them. The Interactions transport
    has no penalty or log-probability field and fails with `model.unsupported_config`.
  - `adk_gemini::GenerationConfig` gains `seed`, `presence_penalty`, `frequency_penalty`,
    `response_logprobs`, and `logprobs`; `adk_gemini::Candidate` gains `logprobs_result`
    (new `LogprobsResult`, `TopLogprobsCandidates`, `LogprobsCandidate`) and `avg_logprobs`.
    `Candidate` now derives `Default`. Struct literals of either type must set the new
    fields or end with `..Default::default()`.
  - `OllamaModel` sends `stop_sequences`, `seed`, and both penalties in `options`, and
    `top_logprobs` as `logprobs` and `top_logprobs`, returning the tokens in
    `provider_metadata["logprobs"]`.
  - OpenRouter's Responses mode fails on `seed`, which its API does not have, with
    `model.unsupported_config`. Streamed Chat Completions and Responses answers return the
    requested log probabilities in `provider_metadata["logprobs"]`.
