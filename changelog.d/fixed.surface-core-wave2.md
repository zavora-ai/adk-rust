- **adk-surface**: the engine's wave 2 review fixes (spec 3.3 §6, §7.1, §7.2, §7.4, §8, §11):
  input queued behind a decision or a running turn is sent, oldest first, whichever surface
  answered and however long the turn ran, and a later message never overtakes it; a recorded
  answer always resumes its call; "Always allow" is never offered for a decision that needs
  more than one approver, grants its rule only once the request is approved, says what the rule
  covers, and records `harness.rule.created`; Stop all withdraws a spoken "allow once" in its
  cancel window; the tray and voice keep Deny on high-risk decisions; a feed over the control
  plane waits for the kernel's events instead of re-reading it in a loop; a decision the kernel
  itself asks, such as an agent's proposed routine, reads in plain words with a `summary`.
