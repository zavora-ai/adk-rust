- **Approvals sealed to the drawing the person saw** (`adk-core`, `adk-harness`,
  `adk-surface`): `ApprovalSeals` seals a person's yes to a drawn form's call with an HMAC over
  the tool, the approved arguments, what the card showed, the drawing's digest and version,
  the approver and an expiry (`surface.drawn.sealed`); `RequestedVia` carries the drawing's
  `digest`. Right before the call runs, `RunConfig::requested_call_seal`
  (`adk_core::RequestedCallSeal`, `HarnessAgentBuilder::requested_call_seal`) checks it: a call
  that changed since, or whose seal was forged, spent or expired, is refused, and each seal
  runs one call (`SurfaceEngineBuilder::approval_seals`, `ApprovalSeals::strict`). Decision
  cards cap every line (`CARD_NAME_LIMIT`, `CARD_HEADING_LIMIT`, `CARD_LINE_LIMIT`).
