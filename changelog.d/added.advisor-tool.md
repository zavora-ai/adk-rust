- **Advisor consultation tool** (`adk-tool`): `AdvisorTool` lets an agent ask a stronger or
  different model for advice mid-task with a `question` and `context`. The advisor sees the
  recent conversation, runs with no tools, and its call counts against the run budget and is
  committed in the spend ledger under `advisor:{agent}`. Consultations are capped per run
  (3 by default), and the kill switch refuses or ends one. See
  `docs/official_docs/tools/advisor.md`.
