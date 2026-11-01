- **Gemini replays only Gemini parts** (`adk-model`): a server-tool payload that is not a
  Gemini `toolCall`, `toolResponse`, `executableCode` or `codeExecutionResult` part is left
  out instead of being wrapped as a `toolCall`, so another provider's payload never reaches
  Gemini. Code execution restored by the Vertex AI session service replays as native
  `executableCode` and `codeExecutionResult` parts with their thought signatures.
