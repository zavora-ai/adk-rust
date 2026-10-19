- **Plugin `on_model_error` runs** (`adk-agent`, `adk-plugin`, `adk-core`): `LlmAgent`
  reports a failed model call or response stream to the run's `InvocationHooks::on_model_error`,
  which `PluginManager` forwards to each plugin's `on_model_error`, before it fails the turn.
  A response a plugin returns answers the turn in the failed call's place, so a model-fallback
  plugin works. Budget and spend refusals are not reported.
- **Retry-reflect budgets are per invocation** (`adk-retry-reflect`): one `RetryReflectPlugin`
  serving many users no longer stops reflecting for everyone once one invocation used up a
  budget. Counts are kept per invocation, a tool's success clears its counts, and with
  global tracking a success closes the tool's circuit.
- **`on_event` sees every event a run yields** (`adk-runner`): a `before_run` early exit, the
  answers a cancelled run gives its calls in flight, and the budget stop event now pass
  through the plugins' `on_event` before they are persisted.
- **A `before_agent` answer is stored under `output_key`** (`adk-agent`): the content a
  before-agent callback returns for an `LlmAgent` is saved to `state[output_key]`, as a model
  answer is. `output_key` keeps the final turn's text, as the docs now state.
- **`ParallelAgent` stops its branches on escalate** (`adk-agent`): a branch that escalates
  stops the other branches where they are, and the escalation ends an enclosing `LoopAgent`.
- **A poisoned session lock keeps its data** (`adk-runner`): the runner's session view uses
  the data behind a lock poisoned by a panic, instead of returning an empty history or
  dropping events and state.
- **Agent tools nest without overflowing the stack** (`adk-agent`): `LlmAgent` runs each call
  to an agent-delegation tool on a Tokio task of its own, so a delegate's agent no longer
  stacks its frames on the caller's. Three nested `AgentTool`s overflowed a 2 MiB worker
  thread in debug builds (`thread 'tokio-rt-worker' has overflowed its stack`); the stack a
  run needs no longer grows with the nesting depth. Dropping the call aborts the task.
