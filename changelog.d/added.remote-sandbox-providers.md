- **Remote sandbox providers behind `SandboxClient`** (`adk-sandbox`, `adk-harness`):
  `E2bClient` (feature `remote-e2b`) runs workspaces in E2B Firecracker microVMs over the E2B
  REST API and the in-VM `envd` daemon, and `DaytonaClient` (feature `remote-daytona`) in
  Daytona sandboxes over the Daytona REST and toolbox APIs. Each environment is created with
  a provider-side lifetime, destroyed by `stop` or when its client drops, and its requests
  carry deadlines. `SandboxSession` gains `exec(CommandRequest)` with a per-command timeout,
  environment and working directory, and `upload` and `download`; `SandboxClient` gains
  `provider`, `capabilities` and `pause`, all with defaults. `SessionBackend` runs
  `SandboxTool` and `adk-code` requests in any client's environment. `ShellIsolation::Remote`
  (`adk-harness` feature `remote-shell`) runs `bash` there through the container shell's
  governed path: approvals, audit records, the kill switch, and the run's remaining wall
  time. The live runs against E2B and Daytona are `#[ignore]`d tests pending credentials.
