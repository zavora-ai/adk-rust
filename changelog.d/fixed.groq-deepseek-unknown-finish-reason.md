- **Groq and DeepSeek report an unknown finish reason as `FinishReason::Other`**
  (`adk-model`): a complete Groq response, and complete or streamed DeepSeek responses,
  read a finish reason the client does not know, such as DeepSeek's
  `insufficient_system_resource`, as `Stop`, so a cut-off answer looked finished. They now
  read it as `Other`, as the other Chat Completions clients do.
