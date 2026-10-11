- **`CodeActAgent` records non-idempotent tool calls in the action ledger**
  (`adk-agent`): a `ToolEffect::NonIdempotent` call from generated code is begun in
  `RunConfig::action_ledger` after the governed path authorizes it and completed after
  it runs, as in `LlmAgent`. A call replayed from a checkpoint — crash recovery from the
  write-ahead checkpoint, or a repeated resume of a confirmed call — is answered from the
  ledger instead of executing again, so a recovered program no longer repeats a payment.
  - A ledger answer is raised into the script with the `LlmAgent` payload
    (`already_succeeded`, `already_failed`, or `outcome_unknown`).
  - A non-idempotent call that times out or panics is raised as `outcome_unknown`, with
    or without a ledger, and keeps its begun record.
  - `LlmAgent` and `CodeActAgent` share one ledger code path.
