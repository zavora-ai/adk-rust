- **The console shows a decision before the call's output** (`adk-cli`, `adk-harness`): the
  harness REPL printed `[approved] bash` when the call's response arrived, after the command's
  streamed output. The console prompter now prints `[approved] <tool>` or `[denied] <tool>` when
  you answer, and a rule hit (`[allowed by rule N] …`) after the call's line; the response
  prints a decision only when it was taken elsewhere (the approvals inbox, a resumed run). A
  call's streamed output starts with a `[<tool> <short call id>]` header. `RuleHit` carries the
  call's `function_call_id`.
