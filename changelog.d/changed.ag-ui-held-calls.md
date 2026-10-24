- **AG-UI held calls end as interrupts** (`adk-server`): a protocol-native AG-UI stream no
  longer sends a held call's placeholder `TOOL_CALL_RESULT` or the hold's placeholder text
  message; the run ends with an interrupt for the call instead, and the call's result
  arrives against the same `toolCallId` in the run that resumes it. `GET
  /api/ui/capabilities` lists `tool_call_interrupts` and `interrupt_resume` for `ag_ui`
  (spec 3.3 §10 S2).
