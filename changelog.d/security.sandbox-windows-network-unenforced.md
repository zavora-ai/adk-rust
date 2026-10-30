- **The Windows sandbox says it leaves the network open** (`adk-sandbox`): the
  `SandboxPolicy` rustdoc stated that `allow_network = false` blocks all network access on
  every platform, while the Windows restricted-token enforcer has no network filter and
  never applied it. The rustdoc now states which enforcer applies `allow_network` and
  `network_rules`, and `WindowsEnforcer::run_confined` and `spawn_contained` log a
  `tracing::warn`, once per process, for the first policy with `allow_network = false` or
  with `network_rules`, as Seatbelt and bubblewrap do for the rules they cannot apply.
  Policies that must keep a process off the network are enforced only by Seatbelt and
  bubblewrap.
