- **Every transfer obeys the source agent's restrictions** (`adk-runner`, `adk-core`): the
  runner checks a transfer a tool or a custom agent requests, not only the model's
  `transfer_to_agent` call, against the agents the source may transfer to — its sub-agents,
  its parent unless `disallow_transfer_to_parent`, and its parent's other sub-agents unless
  `disallow_transfer_to_peers`. A transfer to any other agent in the tree is refused and
  logged, and control stays with the source; a root with `strict_transfer_policy()` fails the
  run with `agent.transfer.target_forbidden`. `Agent` gains `allows_transfer_to_parent` and
  `allows_transfer_to_peers` (default `true`); `LlmAgent` and `CodeActAgent` report their
  flags.
- **`after_agent` runs on every exit except a failure or a held call** (`adk-agent`):
  `LlmAgent` runs its after-agent callbacks, and the run's `after_agent` hooks, after a
  transfer, a turn-ending tool result and a cancellation, as it does after an answer. A stream
  dropped mid-turn runs them on the Tokio runtime and discards any content they return, except
  for a durable worker's attempt, which the recovering worker finishes. A failed agent still
  skips them, and a held call runs them when a later run resumes it.
- **`after_run` runs once, after the run's last event** (`adk-runner`): the runner runs the
  plugins' `after_run` when the run stream ends, including paths that ended without it, such
  as a refused or failed transfer, and for a consumer that drops the stream (a durable
  worker's dropped attempt excepted). A dropped run first answers its tool calls in flight
  outcome-unknown and persists the answers.
- **`${x}`, `\{x}` and `{{x}}` in instructions stay literal** (`adk-core`):
  `inject_session_state` no longer substitutes a brace after `$` or `\`, or doubled braces,
  so instructions that quote shell, JavaScript or template code no longer fail with
  `State variable '…' not found`. `{var}`, `{var?}` and `{artifact.name}` are unchanged.
- **`RetryTracker` keeps counts per invocation** (`adk-retry-reflect`): `increment` and `get`
  take the invocation ID, tool name and call key instead of one `"{tool}:{call_id}"` key, and
  `total` takes the invocation ID. `RetryTracker::with_capacity`, `reset_tool`,
  `tracked_invocations` and `GlobalRetryTracker::record_success` are new; a tracker holds at
  most `DEFAULT_TRACKED_INVOCATIONS` (1,024) invocations by default and forgets the oldest
  first.
