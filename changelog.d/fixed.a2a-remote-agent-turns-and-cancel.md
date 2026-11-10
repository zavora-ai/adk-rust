- **`RemoteA2aV1Agent` sends turns and ends remote tasks as adk-go's remote agent does** (`adk-server`, `a2a-v1`):
  - **The remote task is cancelled when a turn ends before it does:** the caller stops reading, the stream breaks, or the remote returns a task still at work. The cancel is sent on its own, for at most 5 seconds. A task that ended or waits on this side is left alone.
  - **A function response the remote never asked for goes as text** (`Tool <name> returned: <json>`), not as a function response the remote cannot pair with a call.
  - **The waiting remote task continues only with an answer to one of its calls.** A plain follow-up starts a new task in the same remote context.
  - **Nothing new since the agent's last reply sends no request.** The turn's user content seeds the message only when the session holds nothing for the agent yet. Before, it was sent again on every empty turn.
  - **Thoughts go out marked `adk_thought`.**
  - **Streaming follows the run.** The agent streams when `RunConfig::streaming_mode` is not `None` and the card says the remote streams. A configured `streaming` overrides the run, never the card.
  - **`RemoteA2aV1Config::send_configuration` is sent with every message** (`historyLength`, accepted output modes, a push config). `RemoteA2aV1Config::new` builds a config from a card.
  - **`SendMessage` is not retried automatically,** so a timeout cannot start a second remote task.
  - **`A2aV1Client` gains `stream_message_with` and `send_message_rest_with`.**

  Migration: add `send_configuration: None` to `RemoteA2aV1Config` struct literals, or build the config with `RemoteA2aV1Config::new`.
