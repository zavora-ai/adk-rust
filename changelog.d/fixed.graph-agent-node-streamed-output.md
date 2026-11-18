- **An agent node's streamed answer reaches the next node whole** (`adk-graph`): the default
  `AgentNode` output mapper writes one `messages` entry per complete response, read from the
  agent's non-partial events as the runner persists them, instead of one entry per streamed
  chunk. The next node's default input mapper receives the whole reply rather than its last
  chunk, a `GraphAgent` answers with it, and a streamed run writes the same state as an
  unstreamed one. Tool progress output is no longer written to `messages`.
