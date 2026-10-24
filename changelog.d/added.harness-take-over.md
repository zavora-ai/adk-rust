- **Take over** (`adk-harness`; spec 3.3 §8.2, §10 H2): a tool asks the person for a password
  or a one-time code with `take_over::TakeOver::ask`; the request reaches surfaces through the
  approval inbox as a take-over entry (`TakeOverRequest`), and `TakeOver::provide` delivers
  what the person typed on the phone or the web home to that one call as a `ScopedToken`. The
  value never enters the inbox, a session event, the action ledger, the audit log or a log
  line, and the model sees `[secret typed by the person]` wherever the call's result or error
  held it. `harness.take_over.requested`, `provided` and `ended` audit records name who and
  where. `TakeOver` is also a `CredentialBroker`; `HarnessAgentBuilder::take_over` wires it.
