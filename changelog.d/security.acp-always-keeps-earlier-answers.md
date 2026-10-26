- **An ACP "always" answer never overrides an answer given earlier in the same turn**
  (`adk-acp`; spec 3.3 §10 A1): when the person answers one call of a model turn and then
  picks "Always allow" or "Always deny" the other way for a later call, the server remembers
  the rule only once the turn's calls have run, so a call the person denied is never run by
  the new allow rule, and one they allowed once is never refused by the new deny rule.
