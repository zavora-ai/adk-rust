- **The A2A v1 server honours its card and accepts any data part** (`adk-server`):
  - **Capability checks:** an operation the agent card says is off is refused with the
    spec's error. With `streaming: false`, `SendStreamingMessage` and `SubscribeToTask`
    get `UnsupportedOperationError`. With `pushNotifications: false`, push config calls and
    a push config sent with a message get `PushNotificationNotSupportedError`. With
    `extendedAgentCard: false`, `GetExtendedAgentCard` gets
    `ExtendedAgentCardNotConfiguredError`. A capability the card leaves unstated is not
    checked. `A2aV1Server`'s card states all three: push is on only with a push sender,
    and the extended card is off.
  - **Data parts:** a data part that is not a tagged or wrapped function call or response
    no longer fails the whole message with -32602. The agent reads its JSON as text,
    marked `<a2a_datapart_json>…</a2a_datapart_json>` as adk-go marks it
    (`v1::convert::datapart_text`).
