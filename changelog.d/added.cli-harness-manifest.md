- **`adk-rust` tells the harness what the person can type** (`adk-cli`): every slash
  command, subcommand with its long help, and the flags of the REPL, `rules add`, `memory`,
  `sessions`, `up` and `wake`, generated from the command table and the clap tree, reach the
  model in the `<harness>` block and `harness_help`; a slash line that names no command goes
  to the agent after one line on the console, `no command /x: sent to the agent`
  (spec 3.1 §7.25). `adk_cli::harness::slash_command_entries` lists the REPL's commands.
