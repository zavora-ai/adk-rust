- **`Runner::close`** (`adk-runner`, `adk-core`): releases what a runner's agents and plugins
  hold. It calls the new `Agent::close` on the root agent, whose default closes each
  sub-agent and which `LlmAgent` and `CodeActAgent` use to close their toolsets through the
  new `Toolset::close` hook, then closes the plugins. `Runner::allowed_transfer_targets`
  returns the agents an agent may transfer to.
- **`agent_lifecycle` example** (`examples/agent_lifecycle`): transfer restrictions on tool
  transfers, literal braces in instructions, after-agent and after-run exits, a model-fallback
  plugin, per-invocation retry-reflect budgets, `output_key`, parallel escalation, `on_event`
  coverage and `Runner::close`, against a scripted OpenAI-compatible upstream and live Gemini.
