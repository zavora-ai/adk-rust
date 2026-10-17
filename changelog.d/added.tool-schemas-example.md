- **`tool_schemas` example** (`examples/tool_schemas`): `LlmAgent` + `Runner` with a
  `#[tool]` tool against a scripted OpenAI-compatible server, a scripted Gemini server, and
  live OpenAI and Gemini, asserting that unknown and out-of-range arguments are rejected with
  an error the model corrects and the corrected call runs once, that Gemini receives the
  `$ref` description and bounds and accepts them, that duplicate tool names fail at build,
  and that `ScriptedLlm` records each request.
