- **Hooks around `RemoteA2aV1Agent` turns, as adk-go's remote agent offers them** (`adk-server`, `a2a-v1`): `RemoteA2aV1Config::hooks` takes a `RemoteAgentHooks`.
  - **`before_agent` and `after_agent`** run around each turn, after the run's plugins (`InvocationHooks`), as an `LlmAgent`'s agent callbacks do. Content from `before_agent` is the turn's answer, and content from `after_agent` is one more event.
  - **`before_request`** may change the message, or answer instead of the remote. No request is made when it answers.
  - **`after_request`** sees each event of the turn and may replace it. An error ends the turn.
  - **`event_converter`, `from_wire_part` and `to_wire_part`** replace the default conversions.
  - **`remote_task_cleanup`** ends an unfinished remote task instead of the default cancel.
  - **`card_provider`** gives the agent card for each turn.
  - **`allow_transfer_to_agent`** follows the remote's `adk_transfer_to_agent`. It is off by default.

  Migration: add `hooks: Default::default()` to `RemoteA2aV1Config` struct literals, or build the config with `RemoteA2aV1Config::new`.
