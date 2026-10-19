- **`openai` enables `async-openai`'s `byot` feature** (`adk-model`): Cargo unifies features
  across a build, so an application that also calls `async-openai` 0.41 directly gets the `byot`
  build, in which the Chat Completions and Responses `create_stream` methods no longer set
  `stream: true`. Set `stream: Some(true)` on requests passed to `create_stream`.
