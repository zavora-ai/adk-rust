- **`ApprovalRequest::shows_call_args`** (`adk-core`; spec 3.3 §7.2): whether a request's
  `argsRedacted` are the call's own arguments. The approvals API adds it to every request it
  serves as `showsCallArgs`, beside the omitted fingerprint, and `adk-surface` offers **Edit**
  only where it is true, on every surface: an edit replaces the call's arguments whole, so one
  that started from a redacted view would submit its masks.
