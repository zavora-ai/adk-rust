- **`graph_correctness` example** (`examples/graph_correctness`): joins after conditional
  branches and revise loops, a rate-limited `LlmAgent` node, a graph tool called twice,
  subgraphs and `run_node` children in loops, parent retries, timeout policies, and fan-in state
  size, through `GraphAgent` and `Runner` over a SQLite checkpointer against a scripted
  OpenAI-compatible server, plus one live Gemini node.
- **Checkpoint thread helpers** (`adk-graph`): `adk_graph::agent::session_thread_id` returns the
  thread a `GraphAgent` runs a session on, and `tool_call_thread_id` the thread of one graph tool
  call. `NodeContext` implements `Clone`.
