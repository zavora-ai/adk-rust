- **Workspace trust is bound to the instruction files' content** (`adk-harness`): a trust
  decision records `ProjectInstructions::instructions_digest`, the digest of every project
  instruction file's path and content, and covers only those files. An `AGENTS.md` edited,
  added or removed after the decision is not loaded, and each run records
  `harness.instructions.changed` until the person trusts the workspace again. A decision
  recorded before this change covers no files. New `WorkspaceTrust::instructions_digest`,
  `with_instructions_digest` and `covers` (spec 3.4 §10 P2; spec 3.1 §7.7, §9.2 I31).
