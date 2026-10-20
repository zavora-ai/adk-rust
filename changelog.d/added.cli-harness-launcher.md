- **`adk_cli::harness`** (`adk-cli`, `harness` feature): `HarnessLauncher` runs a
  `HarnessAgent` as the REPL (`run`, `run_console_directly`), one turn at a time
  (`run_once` returns the calls held for a decision) or behind the server (`build_app`,
  `run_serve_directly`); `ConsolePrompter` implements `ApprovalPrompter` over the shared
  `ConsoleIo` reader; `HarnessArgs` and `ResolvedHarnessArgs` are the shared flags and their
  config-file precedence; `LoopbackOperatorExtractor` and `BearerTokenExtractor` identify
  served callers. The binary adds `sessions list|show|rm`, `rules list|add|rm` (with
  `--operator` rules for any declaration of a publisher) and `approvals list|approve|deny`,
  locally or against a served harness with `--server URL --token T`.
