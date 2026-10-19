- **Every transfer obeys the source agent's restrictions** (`adk-runner`, `adk-core`): the
  runner checks a transfer a tool or a custom agent requests, not only the model's
  `transfer_to_agent` call, against the agents the source may transfer to — its sub-agents,
  its parent unless `disallow_transfer_to_parent`, and its parent's other sub-agents unless
  `disallow_transfer_to_peers`. A transfer to any other agent in the tree is refused and
  logged, and control stays with the source. `Agent` gains `allows_transfer_to_parent` and
  `allows_transfer_to_peers` (default `true`); `LlmAgent` and `CodeActAgent` report their
  flags.
- **`after_agent` runs on every exit except a held call** (`adk-agent`): `LlmAgent` runs its
  after-agent callbacks, and the run's `after_agent` hooks, after a transfer, a turn-ending
  tool result and a cancellation, as it does after an answer. A stream dropped mid-turn runs
  them on the Tokio runtime and discards any content they return; a dropped durable attempt
  leaves them to the worker that resumes it. A failed agent still skips
  them, and a held call runs them when a later run resumes it.
- **`after_run` runs once, after the run's last event** (`adk-runner`): the runner runs the
  plugins' `after_run` when the run stream ends, after a budget stop event, and for a
  consumer that drops the stream; paths that ended without it, such as a refused or failed
  transfer, now run it. A dropped run's tool calls in flight are answered outcome-unknown,
  persisted and audited before `after_run`. A durable attempt (`RunConfig::durable_run_id`)
  that its worker drops writes nothing: the worker that recovers the run answers its calls
  and finishes it.
- **`${x}`, `\{x}` and `{{x}}` in instructions stay literal** (`adk-core`):
  `inject_session_state` no longer substitutes a brace after `$` or `\`, or doubled braces,
  so instructions that quote shell, JavaScript or template code no longer fail with
  `State variable '…' not found`. `{var}`, `{var?}` and `{artifact.name}` are unchanged.
- **`RetryTracker` keeps counts per invocation** (`adk-retry-reflect`): `increment`, `get`
  and `total` take the invocation ID and tool name; `reset_tool` and `tracked_invocations`
  are new, and the tracker holds at most `DEFAULT_TRACKED_INVOCATIONS` (1,024) invocations.
  `GlobalRetryTracker::record_success` clears a tool's failures.
