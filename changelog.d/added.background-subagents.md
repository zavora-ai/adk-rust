- **Background sub-agents** (`adk-core`, `adk-agent`, `adk-server`, `adk-kernel`, `adk-runner`,
  `adk-cli`): a parent agent's `start_subagent` runs another agent as a durable run — a
  `subagent` process on the agent kernel — and returns at once. The parent is told when it
  finishes, by a steering message to its running turn or a new turn of its session, once
  across crashes; `message_subagent` queues follow-ups, `cancel_subagent` stops it, and
  `subagent_status` reports `queued`, `running`, `waitingForHuman`, `parked`, `completed`,
  `failed` or `cancelled`. Each sub-agent runs on a budget slice carved from the parent's run
  (`BudgetTracker::carve`), acts for the parent's subject with the parent in its delegation
  chain, and can call only tools its parent has (`ToolCeiling`); fork mode starts it from a
  copy of the parent's session. `adk_core::subagent` holds the contract, `adk_agent::subagents`
  the tools, `adk_server::background::DurableSubagents` the host without a kernel, and
  `KernelBuilder::subagents` the kernel's, with `KernelEvent::Subagent` on the bus and
  `adk-rust ps` naming each sub-agent's parent and any held call as `waiting for a human`.
  `RunBudget` serializes, `RunBudget::narrowed` combines two budgets, an agent run's input
  narrows the runner's limits under `runLimits` (`RunLimits`), and `Runner::tool_policy`
  returns the runner's policy. `examples/background_subagents` runs the lifecycle live on
  Anthropic, including a `kill -9` and resume.
