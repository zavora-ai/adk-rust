- **Long-running calls are listed by call id** (`adk-agent`): `LlmAgent` put tool names in
  `Event::long_running_tool_ids`. It now lists each long-running call by its call id, as
  adk-python and adk-go do, and falls back to the tool name for a call without an id. The
  runner and the A2A server read either form.
- **Parallel tool results reach Gemini in one content** (`adk-model`): the results of a
  turn of parallel calls went to Gemini as one content each. Gemini expects one content
  that answers every call, so adjacent `function` contents are now joined.
- **The REST API accepts function calls and responses** (`adk-server`): a run request's
  message parts took only `text` and `inlineData`, so a client could not answer a
  long-running call over REST. Parts now also take `functionResponse` and `functionCall` in
  Gemini's shape, `{id, name, response}` and `{id, name, args}`. A function response sent
  with the call's id resumes the agent with the answer. `MessagePart` gains
  `function_call` and `function_response` and derives `Default`.
