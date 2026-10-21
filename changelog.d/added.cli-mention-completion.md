- **Tab completion at the console** (`adk-cli`, spec 3.1 §7.21, §8.1): `ConsoleIo`
  completes `@server:uri` mentions from the resources the harness's MCP servers publish
  (refreshed after each turn and after `/mcp resources SERVER`) and slash commands at the
  start of a line, in the harness REPL and the chat REPL. `ConsoleIo::set_mention_completions`,
  `set_command_completions` and `completions_for` are the surface.
