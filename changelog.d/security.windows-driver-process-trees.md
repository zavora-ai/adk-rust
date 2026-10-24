- **MCP servers and kernel drivers end with their whole process tree on Windows**
  (`adk-tool`, `adk-kernel`): `McpServerManager` starts each stdio server suspended and runs
  it only once it is in a Job Object of its own, which closes with the server's transport
  (spec 3.2 §10 W1). Stopping, restarting, updating or removing a server, a crash's cleanup,
  a dropped manager and the managing process exiting now end every process the server
  started, where before a background process it started ran on. A server that cannot be
  placed in a Job Object is not started. Every kernel driver runs this way, sandboxed or not;
  the kernel records it as `DriverRecord::process_tree` (`DriverProcessTree::JobObject`) and
  in the `kernel.driver.started` record, and `adk-rust drivers list` shows it.
