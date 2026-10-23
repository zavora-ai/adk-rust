- **Drivers in the agent kernel** (`adk-kernel`, `adk-harness`, `adk-cli`; spec 3.2 §7.7):
  MCP servers the kernel supervises as child processes under manifests the user approved.
  - `DriverManifest` (TOML) names the command, environment, partitions, `sandbox = "os" |
    "container" | "unsandboxed"`, restart backoff and each tool's `EffectClass` (`read`,
    `write_local`, `schedule_with_others`, `send_external`, `publish_public`, `financial`,
    `delete`, `other`); a workspace's manifest may only restrict the user's.
  - `DriverSupervisor` mounts a manifest as an approval (`kernel.driver.mounted`), runs each
    driver on its own `McpServerManager` through a stdio launcher on
    `adk_sandbox::sandbox::sandboxed_command`, recording `Unsandboxed { reason }` where no
    enforcer runs, with the environment cleared to an allowlist and each `secret://` value
    minted by the credential broker into a file the server reads through `<NAME>_FILE`.
    Status changes become `KernelEvent::Driver` and `kernel.driver.*` records.
  - Each tool's declaration digest is pinned at the first listing after approval; a changed
    or new tool is stale, `NeedsReapproval`, its standing rules stop matching, and
    `DriverPolicy` asks before every call of it; `reapprove` pins the new declarations and
    moves the re-approving person's rules (`HarnessRulePins`).
  - A driver call answered with a question parks as a held call with its `DriverPark`
    stored as the run's checkpoint; the decision, in this process or after a kernel
    restart, re-issues it once (`mrtr`, `task`, `legacy`).
  - `DriverStore` on in-memory, SQLite, PostgreSQL and Redis with
    `driver_store_conformance_tests!`; `/api/kernel/drivers` routes and `ControlClient`
    methods; `adk-rust drivers list|mount|unmount|restart|reapprove`, and `adk-rust up`
    serves the mounted drivers' tools.
  - `adk-harness`: `driver_tool_kind` maps a class onto a trusted `ToolKind::External`,
    `DriverClasses` and `HarnessAgentBuilder::kernel_drivers` attach a partition's drivers,
    `OriginClasses` and `ToolClassRegistry::with_origin_classes` class tools by an approved
    declaration, and `HarnessAgentBuilder::add_operator_policy` composes another policy layer.
