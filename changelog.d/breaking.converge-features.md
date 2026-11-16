- **The 19 alias features are removed** (`adk-rust`, `adk-model`, `adk-audio`, `adk-auth`,
  `adk-gemini`, `adk-telemetry`): `cerebras`, `fireworks`, `mistral`, `perplexity`,
  `sambanova` and `together` → `openai` with an `OpenAICompatibleConfig` preset;
  `whisper-onnx`, `distil-whisper`, `moonshine` and `chatterbox` → `audio-onnx`;
  `action-code`, `action-db` and `action-email` → `action`; `action-rss` → `action-http`;
  `a2a` and `agent-registry` → `server`; `cli-deepseek` and `cli-groq` → `cli`; `personas` →
  `eval`. Each sibling now compiles what its aliases did, so `adk_rust::registry` comes with
  `server` and `adk_rust::personas` with `eval`. `adk-model` drops its seven preset aliases
  (`xai` included), and the no-op features `adk-audio/streaming`, `adk-auth/adk-server`,
  `adk-gemini/studio` and `adk-telemetry/metrics` are removed. The umbrella has 149
  features; see the migration guide's "adk-rust features" table.
