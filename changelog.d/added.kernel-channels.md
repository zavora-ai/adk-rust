- **The chat channel contract** (`adk-kernel`, feature `channels`; spec 3.3 §10 K1): the
  kernel's side of every chat thread, for platform adapters to build on. `ChannelGateway`
  gives an unpaired account a pairing code and starts nothing; a paired account's message
  starts a turn as its person, keyed by the platform's message id, and the reply returns to
  the same thread; "stop all" freezes the kernel and "resume" is refused. `ChannelDoorbell`
  sends each decision as a `DecisionCard` carrying the agent, a summary with every argument
  value masked and the risk, held through quiet time; chat answers **Allow once** or **Deny**
  at low and medium risk only, through `ApprovalService::decide` at the card's version, and a
  second answer is told "decided already". Paired accounts live in a `ChannelBindingStore`
  (in-memory, SQLite, PostgreSQL, Redis, with `channel_binding_store_conformance_tests!`),
  accepted with `adk-rust pair accept CODE`. Adapters implement `ChannelSink` and pass
  `channel_contract_tests!`; `InMemoryChannel` is the test channel. Every chat action writes
  a `kernel.channel` audit record.
