- **Usage types gain a `raw` field** (`adk-anthropic`, `adk-gemini`): `adk_anthropic::Usage`
  (`Option<Box<Value>>`), `adk_anthropic::MessageDeltaUsage`, `adk_gemini::UsageMetadata` and
  `adk_gemini::interactions::Usage` (`Option<Value>`) keep the usage object as received.
  Struct literals add `raw: None`; of the four, only `interactions::Usage` implements
  `Default`. `PartialEq` compares `raw`, so a value parsed from a response no longer equals
  one built in code with the same counts. `raw` is not serialized.
- **`adk_anthropic::Usage` is no longer `Copy`** (`adk-anthropic`): clone it where it was
  copied.
- **`adk_anthropic::BatchResult::Succeeded` boxes its message** (`adk-anthropic`): `message`
  is `Box<Message>`. Field access is unchanged; construction wraps the message in
  `Box::new`, and code that moves it out dereferences the box.
