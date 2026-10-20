- **Retryable Anthropic streams that end before `message_stop`** (`adk-model`):
  a missing-terminal EOF now reports `Unavailable` instead of a legacy internal
  error, so the existing retry policy can recover incomplete responses. Partial
  output remains partial, and completed messages retain their normal answer and usage.
