- **Claude's advisor tool** (`adk-anthropic`, `adk-model`, `adk-tool`): `AnthropicAdvisorTool`
  declares `advisor_20260301` (`ToolUnionParam::Advisor20260301`, `AdvisorTool20260301`), and
  the client sends the `advisor-tool-2026-03-01` beta while a request declares it or replays its
  results. `AnthropicClient` prices each `advisor_message` iteration at the advisor model's
  rates into `UsageMetadata::cost`, and its cost bound covers `max_uses` advisor calls, so the
  run budget and the spend ledger count consultations and a capped run refuses an unbounded
  advisor.
