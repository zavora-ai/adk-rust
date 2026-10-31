- **Consent gates** (`adk-core`, `adk-runner`, `adk-session`, `adk-memory`): `ConsentGate`
  decides whether a subject — a visitor by default — may have something lasting written for a
  purpose. `ConsentGatedSessionService` keeps unconsented sessions in process memory and drops
  `user:` keys without profile consent; `ConsentGatedMemoryService` skips adds and returns no
  recall without it; `RunConfig::consent_gate` refuses a tool named with `with_tool` until the
  subject consents to its purpose (`consent.required`), refuses runs for a subject being erased,
  and leaves `adk.user_id` off the run's span without analytics consent.
