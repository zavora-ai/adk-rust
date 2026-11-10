- **An agent that can transfer is told what each target does** (`adk-agent`, `adk-runner`):
  the model used to see only the names of the agents it could transfer to, in
  `transfer_to_agent`'s description, so it chose a target by name alone. An `LlmAgent`
  with transfer targets now adds a system instruction after its own instructions. It lists
  each target with its description and says to answer when it fits best, or else call
  `transfer_to_agent` with no other text, and to fall back to the parent agent when none
  fits. adk-python gives its models the same guidance. Sub-agents' descriptions come from
  the agents themselves. For a parent or peer, the runner supplies the description from
  the agent tree (`Runner::describe_agents`). Agents without transfer targets send the
  same request as before.
