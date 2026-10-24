- **A rule remembered mid-batch decides the calls it covers** (`adk-acp`): before each
  permission request the ACP server asks the new `StandingPermissions::standing_decision`
  (default: none), so a call that a rule remembered for an earlier call of the same batch
  covers is decided by the rule instead of being asked about with an answer the rule would
  override. `StandingOffer::with_allow_warning` adds what "Always allow" also permits to
  that option's label only (spec 3.3 §10 A1).
