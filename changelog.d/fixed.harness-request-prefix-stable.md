- **The harness request prefix is byte-stable within a run** (`adk-harness`; spec 3.1
  §7.23, I56): the instruction is assembled once per run and reused by every model call,
  so a `memory_write` mid-run no longer changes the system instruction, and recalled
  long-term facts are appended after the conversation as per-turn text instead of being
  inserted into the system preamble, where Anthropic hoisted them into the cached system
  prompt on every turn. Facts with equal scores recall in a fixed order.
