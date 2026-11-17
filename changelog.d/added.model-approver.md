- **Model-evaluated tool approval** (`adk-core`, `adk-harness`, `adk-cli`): a `ToolReviewer`
  set as `RunConfig::tool_reviewer` approves, denies or escalates a call the `ToolPolicy` holds,
  before the confirmation handler and the approval inbox. `ModelApprover` is the reviewer backed
  by a model: an isolated request with no tools, no history and no tool results, the call and
  the user's recent messages wrapped as untrusted data, a strict JSON verdict with a confidence
  floor, and `ReviewScope` per app, partition class, agent and tool. It never sees a call the
  policy denies, one a `ToolConfirmationPolicy` or a person's request holds, or one already
  pending for a person; a failure or timeout escalates. Every verdict is an audit record with
  the reviewer as actor, an approval or denial is filed in the inbox through
  `ApprovalService::record_review` as decided by `model:<model id>`, and the model call counts
  against the run budget and the spend ledger. `HarnessAgentBuilder::auto_review` applies it to
  what Auto mode holds, never to non-waivable calls; `adk-rust --auto-review` and
  `auto_review = true` in the user config turn it on, including under `adk-rust up`. Off by
  default.
