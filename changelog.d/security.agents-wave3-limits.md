- **Agents a person adds never do more than their definition** (`adk-harness`, `adk-cli`,
  `adk-kernel`): an agent with no web, or listed sites only, no longer gets the model provider's
  own search; listed sites restrict where it fetches and are never fetched without asking; an
  agent in an area mounts only the drivers its definition names and the browser pack only when
  it names it. "Always allow" on an added agent's card, from a terminal attached to `adk-rust up`
  too, pins the rule to that agent and makes none when the agent table cannot be read; copying a
  rule to an agent is recorded as `harness.rule.created`; resuming an agent paused by hand is
  refused outside the web home and the kernel's own terminal; instructions set up by hand are
  screened and redacted as fetched text is (`AgentFactory::screen`,
  `adk_harness::screen_agent_definition`); a name with invisible characters is refused.
