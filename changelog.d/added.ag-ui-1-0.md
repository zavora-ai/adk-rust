- **AG-UI 1.0 run shape** (`adk-server`): a protocol-native run input that names
  `"protocolVersion": "1.0"` gets AG-UI 1.0's lifecycle — the version echoed, a success
  outcome, `STATE_SNAPSHOT` and `MESSAGES_SNAPSHOT` (held calls as the assistant's `toolCalls`)
  before an interrupting `RUN_FINISHED`, `"ag-ui:tool-execution": "server"` on tool calls and
  interrupts, strict event fields (no `null`s, no thread or run on `RUN_ERROR`, nothing after
  it, reasoning in the `reasoning` role), and resumes that must answer every open interrupt
  (`approval.invalid_resume`) with new input refused meanwhile (`approval.interrupts_open`).
  Clients that name no version keep the earlier shape. `/api/ui/capabilities` lists AG-UI
  `1.0` and `protocol_version_negotiation`.
