- **`AgentTool` keeps a delegate's code-execution output** (`adk-tool`): a delegate's
  provider-side tool results, such as Gemini's `codeExecutionResult`, are returned as
  `"server_tool_responses"` beside `"response"`; only the model's text was returned, and
  a delegate whose turn ended on the code's output returned "No response from agent".
