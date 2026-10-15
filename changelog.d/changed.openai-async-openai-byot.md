- **`openai` enables `async-openai`'s `byot` feature** (`adk-model`): Cargo enables a
  feature for every user of a crate in the build, so an application that also calls
  `async-openai` 0.41 directly must set `stream` on its requests, because `create_stream`
  no longer sets `stream: true`.
