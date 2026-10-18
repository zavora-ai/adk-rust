- **`artifacts_eval_memory` example** (`examples/artifacts_eval_memory`): an `LlmAgent`
  and `Runner` on live Gemini, OpenAI, or Anthropic models saving and loading artifacts
  through tools on the in-memory, file, and GCS backends (a scripted GCS JSON API on
  127.0.0.1), an `adk-eval` run graded by a live judge, and memory recall through a
  tool, plus a concurrent-save harness, a writer process killed mid-save, and an
  earlier on-disk layout, asserting distinct versions for concurrent saves, distinct
  storage for `a:b` and `a__b`, intact versions after a crash, no session leaking into
  another's listing, no vacuous eval pass, and ranked keyword recall.
