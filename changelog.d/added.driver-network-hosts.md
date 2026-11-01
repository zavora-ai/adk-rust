- **Driver network allowlists** (`adk-kernel`, `adk-cli`): a driver manifest's
  `network = { hosts = [...] }` limits a sandboxed MCP server to those hosts through an egress
  proxy started with each process (`DriverAccess::network_hosts`, `egress_policy`). Hosts need
  `sandbox = "os"`; such a driver never runs unsandboxed, and a project manifest may only narrow
  them. Driver tools declare `Enforced`, `None` (sandboxed, network denied) or nothing, and
  `adk-rust kernel` shows and saves the hosts. Refs #1281.
