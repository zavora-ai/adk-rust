- **AG-UI carries decisions** (`adk-server`): with `ServerBuilder::with_approval_service`,
  a protocol-native AG-UI run submits the calls it holds for approval to the inbox and ends
  with `RUN_FINISHED` `outcome: {"type": "interrupt"}`, one `tool_call` interrupt per held
  call whose `id` is its inbox entry and whose metadata carries the entry's summary, risk,
  agent and version, and the arguments only for a caller authenticated as the session's
  owner. The next run's `resume` entries are decided through `ApprovalService::decide`
  before the run starts — allow once, edit with `editedArgs`, or deny with a reason — and
  reported as `CUSTOM` `adk.decision` events; a resume the service would refuse ends the
  stream with `RUN_ERROR` carrying its code, such as `approval.version_conflict`, decides
  nothing and is never retried. `RuntimeController::with_approval_service` wires the same
  for a router built by hand. The shapes follow the AG-UI 1.1 draft interrupt lifecycle
  (spec 3.3 §10 S2). See `docs/official_docs/tools/ui-tools.md`.
