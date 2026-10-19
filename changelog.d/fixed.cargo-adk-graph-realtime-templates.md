- **`graph` and `realtime` templates generate what they describe** (`cargo-adk`): both
  generated a plain `LlmAgent` behind the console. `graph` now builds a `GraphAgent` with an
  agent node, an appending `messages` channel, and a `MemoryCheckpointer`, so each turn
  continues the conversation from the checkpointed state. `realtime` opens one Gemini Live
  session through `adk-realtime` and prints the transcript and audio size of each answer;
  it supports `--provider gemini` only and rejects other providers.
