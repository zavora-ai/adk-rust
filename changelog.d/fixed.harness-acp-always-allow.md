- **The harness's ACP "Always allow" says what it allows, and only where it applies**
  (`adk-harness`, feature `acp-server`): the option names where the rule applies ("every
  session") and, as the terminal prompt does, warns on "Always allow" that a rule over
  commands that run project scripts allows whatever they do (spec 3.1 §7.5);
  `HarnessAgent` implements `StandingPermissions::standing_decision` by re-checking a held
  call as the terminal prompt does, so a rule answers only the harness's own approval; and a
  harness built with `require_authenticated_identity`, whose rules never apply to
  unauthenticated ACP runs, offers no "always" and remembers none (spec 3.3 §10 A1).
