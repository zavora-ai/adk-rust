- **`CompiledTeam::close` reaches the coordinator** (`adk-agent`): a compiled team root
  and its member wrappers now forward `Agent::close`, so `Runner::close` on a team root
  closes the coordinator's and every member's toolsets — MCP connections and container
  sessions included. `Harness::close` no longer closes the harness toolset by hand.
