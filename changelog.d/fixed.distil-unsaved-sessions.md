- **A session that ends without saving teaches long-term memory at the next start**
  (`adk-cli`, `adk-harness`, `adk-surface`): the `adk-rust` REPL and terminal in process
  distil, in the background as they start, the five most recent sessions of the user in the
  workspace whose distillation stopped short of their last event — a killed process, a
  closed terminal window — with every fact `assumed`, and say what they added. The terminal
  in process also distils its sessions when the person leaves it, as the REPL does.
  `Harness::distilled_sessions` lists the distillation cursor,
  `adk_cli::harness::distill_unsaved_sessions` runs the catch-up,
  `TerminalSurface::notices_from` shows lines that arrive while the terminal runs, and
  `harness.memory.distilled` carries `catchUp`.
