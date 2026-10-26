- **Agent Memory and Rules says what the kernel has** (`adk-surface`, `adk-cli`, `adk-kernel`):
  records after the log's last signed checkpoint read as not signed yet, never as unchanged;
  numbered cron weekdays read as the kernel runs them (1 is Sunday), so `1-5` is Sunday to
  Thursday in **Coming up**; an answer on record names whose call it was and the surface it came
  from ("You let Finance use send_email, from Telegram."), names a paired device by its name
  and never its id, says "from the tray" for the tray, and is listed once, never beside an
  answer that lost to it; a rule that refuses a call reads as a refusal; a guess names the
  agent that noticed it, not its area; spend outside the listed areas, such as the kernel's own
  upkeep, has a list of its own; each agent's spending is read again only when an area's total
  moves; **Coming up** reads every waiting process; and **Extend** offers thirty days past the
  rule's current end each time it opens. `RecordLine` gains `surface`.
- **A standing rule store without `replace` never brings a revoked rule back** (`adk-harness`):
  `StandingRuleStore::replace`'s default answers `harness.rule.replace_unsupported` instead of
  reading the rules and granting after, so an extension cannot undo a revocation made between
  the two. Every store of the crate already replaces in one step; a custom store implements
  `replace` to support **Extend**.
