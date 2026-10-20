- **The console shows and answers held tool calls** (`adk-cli`): `StreamPrinter` renders a
  call held for confirmation as `[approval needed] <tool> <args> — <reason>` and an applied
  decision as `[approved] <tool>` or `[denied] <tool>`; it used to print only the event's
  text. Console runs of `Launcher` ask at the terminal through the new
  `ConsoleConfirmationHandler`, which implements `decide_approval` with
  `[y] allow once  [n] deny (reason)  [e] edit args  [?] details`: edited arguments and the
  denial reason travel in the `ToolApproval`, and a direct `decide` call offers only
  `[y] allow once  [n] deny  [?] details`. Prompts are serialized across a batch, and every
  call is denied when stdin is not a terminal. Ctrl-C at the prompt denies the call and
  cancels the run. The REPL and every prompt read through one `ConsoleIo` reader thread, so
  an interrupted prompt leaves no blocking stdin read behind, and a console run waits up to
  `CONSOLE_CONFIRMATION_TIMEOUT` (one hour) for an answer unless its `RunConfig` sets another
  `tool_confirmation_timeout`.
