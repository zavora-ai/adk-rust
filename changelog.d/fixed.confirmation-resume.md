- **Approved held tool calls execute when a run resumes** (`adk-agent`, `adk-core`,
  `adk-runner`, `adk-acp`): a call `LlmAgent` held for confirmation no longer depends on
  the model issuing it again. At the start of the next run, before its first model call, each
  held call with a decision (a call-ID decision, a fingerprint approval, or a decision in
  the run's `ApprovalStore`) is dispatched again under its original call ID through the full
  tool path; an approval executes it once and a denial answers it with the confirmation
  refusal. The run's confirmation handler is asked about a held call only when the call is
  not waiting in the run's `ApprovalStore`: a call recorded there, held without a handler or
  after the handler timed out, resumes only on a decision recorded for it. A call without a
  decision stays held across runs. The ACP server's resume runs execute the call
  the client approved the same way.
  - A held call is answered with `confirmation_pending_response` (`"error"` plus
    `"status": "confirmation_pending"`). The real response supersedes it in the projected
    history, so providers receive one response per call ID.
  - New `adk-core` APIs: `held_tool_calls`, `HeldToolCall`, `Session::held_tool_calls`,
    `confirmation_decision_available`, `ApprovalStore::has_decision`,
    `confirmation_pending_response`, `is_confirmation_pending`, `supersede_pending_responses`
    and `CONFIRMATION_PENDING_STATUS`.
  - A user turn with only empty text, the resume shape, is left out of the model request,
    since Anthropic rejects empty text blocks. The "Tool confirmation required" notice event
    is left out of the projected history, so a resumed request does not end on an assistant
    turn, which models without assistant prefill reject.
  - When a resumed call ends the turn (`escalate` or `skip_summarization`) in a run that also
    brought a user message, the run fails with `agent.resume_ended_turn` instead of ending
    without answering it. The message stays saved, and the next run answers it.
