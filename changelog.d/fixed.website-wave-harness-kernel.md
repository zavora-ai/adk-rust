- **A team led by a harness compiles** (`adk-harness`): `HarnessAgent` reports its root's
  `Agent::capabilities`, so a `TeamSpec` whose coordinator is a harness compiles its delegate
  edges; `adk-website`'s `SiteTeam::build` compiles the back office (spec 3.5 §7.19).
- **Earned autonomy keeps the owner's word** (`adk-harness`, `adk-kernel`): a promotion to "act
  with approval" grants the job's standing rules, and each promotion revokes the rules the new
  level does not hold; `AutonomyLadder::sync` keeps a promotion an owner made after the charter
  took effect, so a restart no longer lowers it (spec 3.5 §7.11, A-I13).
- **Approval chains and website agents** (`adk-harness`): a vote from someone without the role
  no longer blocks the chain for everyone after it; a public-partition agent is offered no
  workspace, memory or plan tool; a website agent is offered no `exit_plan_mode` and its mode
  banner says drafts run at Propose (spec 3.5 §7.10, §7.11, A-I2, A-I23).
