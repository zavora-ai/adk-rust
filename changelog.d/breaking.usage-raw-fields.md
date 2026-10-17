- **Usage types gain a `raw` field** (`adk-anthropic`, `adk-gemini`): `adk_anthropic::Usage`,
  `adk_anthropic::MessageDeltaUsage`, `adk_gemini::UsageMetadata` and
  `adk_gemini::interactions::Usage` carry the usage object as received, so a struct literal
  needs `raw: None` (or `..Default::default()` where the type has a default), and a value read
  from a response no longer compares equal to one built in code unless `raw` matches.
  `adk_anthropic::Usage` is no longer `Copy`; clone it where it was copied. `raw` is not
  serialized, so serializing these types keeps the typed shape.
- **`adk_anthropic::BatchResult::Succeeded` boxes its message** (`adk-anthropic`): `message` is
  `Box<Message>`; field access is unchanged and construction wraps the message in `Box::new`.
- **OpenAI Responses usage without `total_tokens` is accepted** (`adk-model`): a usage object
  that left out `total_tokens` was a `model.openai_responses.parse` error; its total is now the
  sum of `input_tokens` and `output_tokens`. A missing detail count reads as `None`, where it
  was `Some(0)`.
