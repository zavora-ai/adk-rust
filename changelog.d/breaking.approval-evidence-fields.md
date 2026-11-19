- **Approval types gain `evidence`** (`adk-core`): `ToolConfirmationRequest`,
  `ApprovalRequest`, `ApprovalRoute` and `GovernedCall` have a new public `evidence:
  Vec<Evidence>` field, so struct literals must add it, and `Tool` gains
  `approval_evidence`, which a tool wrapper forwards to its inner tool.

  Migration: add `evidence: Vec::new()` to struct literals, or build with `new` and
  `with_evidence`; forward `approval_evidence(args)` in tool wrappers as `egress(args)` is.
