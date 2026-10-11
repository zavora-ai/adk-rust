- **Approved held tool calls execute when a run resumes** (`adk-agent`, `adk-core`,
  `adk-runner`, `adk-acp`): a call `LlmAgent` held for confirmation no longer depends on
  the model issuing it again. At the start of the next run, each held call with a decision
  (call-ID decision, fingerprint approval, `ApprovalStore`, or handler) is dispatched again
  under its original call ID through the full tool path; an approval executes it once and a
  denial answers it with the confirmation refusal. A call without a decision stays held
  across runs.
  - The held call's placeholder response is now `confirmation_pending_response` (`"error"`
    plus `"status": "confirmation_pending"`). The real response supersedes it in the
    projected history, so providers receive one response per call ID.
  - New `adk-core` APIs: `held_tool_calls`, `HeldToolCall`, `Session::held_tool_calls`,
    `confirmation_decision_available`, `ApprovalStore::has_decision` (default method),
    `confirmation_pending_response`, `is_confirmation_pending`, and
    `CONFIRMATION_PENDING_STATUS`.
  - A user turn with only empty text, the resume shape, is left out of the model request,
    since Anthropic rejects empty text blocks. The "Tool confirmation required" notice event
    is left out of the projected history, so a resumed request does not end on an assistant
    turn, which models without assistant prefill reject.
  - The ACP server resumes with call-ID decisions only; a call the model re-issues under a
    new ID is asked about again instead of authorized by an earlier answer.
