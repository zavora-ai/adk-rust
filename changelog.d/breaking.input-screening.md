- **`EventActions` has an `input_screened` field** (`adk-core`): a struct literal that names
  every field needs it or `..Default::default()`. It is omitted from serialized events when
  `false`.
- **A nested agent's input guardrails screen every user message** (`adk-runner`, `adk-core`):
  an input guardrail on a sub-agent, workflow step, or transfer target screens each message
  before the runner saves it, so it also governs what the root and earlier steps see, instead
  of applying only when that agent runs. An agent that wraps another without listing it in
  `sub_agents()` overrides the new `Agent::screen_user_input` to delegate to it.
