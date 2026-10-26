- **Refused suggestions are on record** (`adk-harness`): `propose_agent` hands a suggestion over
  its suggester's limits to the kernel, which refuses it and records `kernel.agent.refused`; the
  model's answer still names what was over.
