- **`write_todos` keeps each session's plan in session state** (`adk-agent`, feature
  `coding`): `TodoTool` held one list in process memory shared by every session using the
  agent, so concurrent sessions overwrote each other's plans and a restarted process lost
  them. The list is now written to session state under `harness:todos`
  (`adk_agent::coding::TODOS_STATE_KEY`) through the call's state delta, as
  `{ items: [{ id, content, status }], revision }`, and the tool declares
  `ToolEffect::Idempotent`. `CodingAgent::todos()` and `TodoTool::items()` return the last list
  any session wrote in this process; read a session's plan from its state. Spec 3.1 §10 A1.
