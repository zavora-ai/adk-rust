- **Provider-neutral model fallback** (`adk-model`): `FallbackLlm` tries an ordered list of
  models of any provider and moves to the next one on a configured `FallbackTrigger` — rate
  limits, timeouts, outages, unknown or retired model ids and spent quotas by default;
  refusals, context overflow and auth failures on request. A model that has streamed a response
  keeps the turn. Every attempt counts against the run budget and is committed in the spend
  ledger under its own provider, and a fallback is marked in `provider_metadata["fallback"]`.
  See `docs/official_docs/models/fallback.md`.
