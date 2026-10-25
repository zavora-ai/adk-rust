- **The REPL's slash commands in the full-screen terminal** (`adk-surface`, `adk-cli`, spec
  3.3 §5.2): `/todos`, `/plan`, `/approvals`, `/rules`, `/web`, `/memory`, `/hooks`,
  `/sandbox`, `/effort`, `/mcp`, `/skills`, `/team` and `/cost` run the REPL's own code in the
  shown thread, with completion above the message box (`↑` `↓`, `Tab`, Enter), an output
  pane, and `/help` listing every command. `TerminalSurface::command` adds one
  (`TerminalCommand`, `CommandContext`); `adk_cli::harness::HarnessCommands` runs the REPL's
  commands outside the REPL. A decision key pressed just after its box was answered elsewhere
  says where: "Already answered in Telegram."
- **The REPL's commands and "Always allow" on a kernel** (`adk-kernel`, `adk-surface`,
  `adk-cli`, spec 3.3 §5.2, §7.2): `GET /api/kernel/commands`, `POST
  /api/kernel/commands/run` and `/api/kernel/rules/always-allow/{id}` (with `prepare` and
  `grant`), served over a `CommandHost` (`ControlPlaneConfig::with_commands`) that `adk-rust
  up` builds over its harness, with `ControlClient::{commands, run_command,
  always_allow_offer, prepare_always_allow, grant_always_allow}`. The person only, never a
  paired device; each run recorded with how the caller reached the kernel (`CommandReach`:
  transport, device, calling process); a command that changes something waits while
  everything is stopped. A terminal attached to a kernel runs its commands there, and
  `ControlSource` offers "Always allow" where the kernel makes the rule.
