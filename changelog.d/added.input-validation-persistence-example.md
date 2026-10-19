- **`input_validation_persistence` example** (`examples/input_validation_persistence`):
  `adk-guardrail` input guardrails (`PiiRedactor`, a blocking `ContentFilter`) over SQLite and
  in-memory sessions and the durable worker, against a scripted OpenAI-compatible server and a
  live OpenAI or Gemini scenario. It asserts that a redacted message is saved redacted, that a
  blocked message is never saved, that a recovered run re-screens an unscreened saved turn,
  that a nested agent's guardrails screen what the root sees, and that a resumed call that ends
  the turn does not leave a new message unanswered.
