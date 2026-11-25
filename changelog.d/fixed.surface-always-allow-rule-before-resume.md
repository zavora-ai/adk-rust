- **"Always allow" on a kernel stores its rule before the call resumes** (`adk-surface`,
  `adk-core`): on `adk-rust up` the person's yes woke the held call's run before the surface
  granted the standing rule, so a covered call later in the same turn could ask again, and
  `/rules` right after the call could show no rule. `StoreApprovalService` runs
  `ApprovalResolution` hooks, added with `ApprovalService::add_resolution`, once the deciding
  answer is recorded and before the outcome reaches the call; `LocalSource` grants the prepared
  rule there when the yes is the person's own.
