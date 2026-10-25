- **MCP servers and kernel drivers end with their whole process tree on macOS and Linux**
  (`adk-tool`, `adk-kernel`): `McpServerManager` starts each stdio server in a session and
  process group of its own, which closes with the server's transport (spec 3.2 §7.7, K-I5).
  Stopping, restarting, updating or removing a server, a crash's cleanup and a dropped manager
  now close the server's input, give it 3 seconds to exit, send the group `SIGTERM`, then
  `SIGKILL`, and return once every process in it has exited, where before a process the
  server started ran on. A server no longer has a controlling terminal, so the terminal's
  Ctrl-C does not reach it. Every kernel driver runs this way; the kernel records it as
  `DriverProcessTree::ProcessGroup` (`processTree: "processGroup"`), and `adk-rust drivers
  list` shows `in a process group`.
