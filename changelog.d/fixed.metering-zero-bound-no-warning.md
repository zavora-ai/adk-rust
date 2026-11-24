- **A local model call no longer warns about its missing cost** (`adk-core`): a response
  without a reported cost logged "model response reported no cost; committing the reserved
  bound or estimate" at WARN even when the bound was zero, as it is for mistral.rs and other
  local inference, so every local call warned while committing 0. A zero bound is now logged
  at DEBUG; a positive bound or estimate still warns.
