- **Escalated payments resume from approvals** (`adk-payments`): a payment a payment
  policy escalated checked only `RunConfig::tool_confirmation_decisions` and the
  confirmation handler, and recorded nothing in the approval store. Without a handler the
  approver's inbox never listed it, and an approval decided in the `ApprovalStore` or passed
  as `RunConfig::tool_approvals` never reached it, so the resumed call was held again and the
  payment never ran. The payment tools now decide escalations through
  `adk_core::confirm_tool_call`, for a request bound to the calling agent and the tool's
  origin: a held escalation is recorded as pending and submitted to the approval service,
  and once an approver decides it the next run pays once and consumes the approval. An
  approval that edits the call's arguments refuses the payment.
