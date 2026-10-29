- **Before-model callbacks can own the history** (`adk-agent`):
  `LlmAgentBuilder::retain_prepared_history(true)` makes contents the before-model callbacks
  changed the agent's conversation history for the rest of the turn, so a callback that compacts
  or rewrites the history does not repeat the work on every request. A request the callbacks
  leave unchanged or skip keeps the history as it was. The default (`false`) keeps a callback's
  changes to one request, as before.
