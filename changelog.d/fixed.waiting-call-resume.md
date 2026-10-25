- **A resolved request wakes every run parked on its call** (`adk-core`; spec 3.3 §7.3):
  `StoreApprovalService` wakes the run the request names and every run of the session whose
  `AwaitingApproval` checkpoint names the call, so a run that held the call again after the
  first one was cancelled resumes on the decision.
- **`CodeActAgent` resumes a waiting call whose arguments a plugin rewrote** (`adk-agent`;
  spec 3.3 §7.3, §7.4): the waiting call is checked under the arguments it was held with,
  from its held event, so its decision resumes it and an identical call in a turn beside it
  is recognised. A turn that decides an identical call answers the waiting call and clears
  its checkpoint; a turn beside a waiting call that ends with a result runs the after-agent
  callbacks; a run that resumes a call on its decision and brings a message keeps the
  message in the transcript and every later checkpoint until a model request carries it,
  also when the resumed script finishes or stops at another call that needs a decision.
- **A current-turn agent sends its own turn while a call waits** (`adk-agent`; spec 3.3
  §7.3): an `IncludeContents::None` agent begins its turn at the message before a held call
  only in a run that resumes that call; while the call waits for a decision, each run sends
  the current user turn alone.
- **`Harness::decide` keeps the inbox request for a rule it cannot remember** (`adk-harness`;
  spec 3.3 §7.1, §7.3): the rule is checked before the request is withdrawn, and under a
  kernel the withdrawn request is reported to `Wake::Approval` sleepers with the person's
  decision once it is recorded, not as cancelled before it.
- **`approval.outcome_unsettled` says when a request is still pending** (`adk-core`): a
  request whose withdrawal kept losing to other changes until the deadline is reported as
  still pending, not as resolved by another decision.
