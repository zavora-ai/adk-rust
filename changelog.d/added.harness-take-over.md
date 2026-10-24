- **Take over** (`adk-harness`; spec 3.3 §8.2, §10 H2): a tool asks the person for a password
  or a one-time code with `take_over::TakeOver::ask`; the request reaches surfaces through the
  approval inbox as a take-over entry (`TakeOverRequest`), and `TakeOver::provide` delivers
  what the person typed on the phone or the web home to that one call as a `ScopedToken`. The
  value never enters the inbox, a session event, the action ledger, the audit log or a log
  line: the model sees `[secret typed by the person]` wherever the call's result or error held
  it — as typed or as a tool normalizes it (trimmed, without spaces or dashes, in either case,
  or a code's value without leading zeros), percent- or form-encoded, JSON-escaped, or as a
  number — and progress or events the call streams after the secret arrives are withheld. A
  one-time code reaches the tool without the spaces or dashes it was typed with.
  `harness.take_over.requested`, `provided` and `ended` audit records name who and where; a
  take over ends declined and recorded at the confirmation timeout or shortly before the
  call's tool timeout, and a dropped wait still cancels its entry and records the end.
  `TakeOver` is also a `CredentialBroker` for tools; it never hands a secret to the harness's
  shell. The harness policy denies any tool named `harness:take-over`.
  `HarnessAgentBuilder::take_over` wires it.
