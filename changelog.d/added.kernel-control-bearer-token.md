- **`ControlClient::bearer_token`** (`adk-kernel`): returns the token the control-plane
  client sends, so a caller of another route on the same control plane, such as
  `adk-surface`'s approvals inbox reads, sends the same one.
