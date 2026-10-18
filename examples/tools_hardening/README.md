# Tools, MCP, auth and code-execution hardening

An `LlmAgent` run through a `Runner` on a live model against servers and services that
misbehave. Each scenario prints `PASS`, `FAIL` or `SKIP` and names what it ran against;
the process exits non-zero on any failure.

Faults a real service cannot be made to produce come from local stand-ins started by
the binary: MCP servers in-process over a pipe or as stdio children of the binary
itself, a scripted Skill Registry, an HTTP server that never answers, a counting token
endpoint, a scripted OpenAI-compatible server, and a shell script standing in for the
`docker` CLI. The agents driving them use live models.

```bash
ADK_ENV_FILE=/path/to/adk-rust/.env \
  cargo run --manifest-path examples/tools_hardening/Cargo.toml
```

`--scenario a,b` runs a subset. Keys are read from `ADK_ENV_FILE`, or from the
repository's `.env` beside this directory; `GOOGLE_API_KEY` (or `GEMINI_API_KEY`)
enables the Gemini scenarios and `OPENAI_API_KEY` the live half of
`tm16_integral_float_arguments`. `GEMINI_MODEL` (default `gemini-3.5-flash-lite`) and
`OPENAI_MODEL` (default `gpt-5.6-terra`) override the models. A scenario whose key is
missing is skipped.

## Scenarios

| Scenario | Runs against | Passes when |
|----------|--------------|-------------|
| `tm1_self_referencing_schema` | Live Gemini, in-process MCP server | A 200-byte schema whose definition refers to itself three times normalizes in under a second, Gemini calls the tool, and a concurrent Gemini agent, on its own task, finishes in under 4 seconds |
| `tm2_skill_registry_names` | Live Gemini, scripted Skill Registry | Asked to load `projects/victim-corp/.../skills/payroll`, the `SkillToolset` sends no request outside its own project and the skill's `wire_funds` tool never runs |
| `tm3_manager_hung_handshake` | Live Gemini, stdio MCP children under `McpServerManager` | While one server never answers `initialize`, the other's status and tools answer and Gemini uses them; the hung server ends `FailedToStart` after the 30-second startup timeout |
| `tm6_empty_mcp_result` | Live Gemini, in-process MCP server | A successful call with no content reaches the model as `{"output": ""}`, not an error, and runs once |
| `tm9_huge_mcp_description` | Live Gemini, in-process MCP server | A 1 MB tool description reaches the agent as at most 16 KiB, and Gemini calls the tool |
| `tm10_function_tool_validation` | Live Gemini | A `FunctionTool` whose schema requires multiples of five never runs with the 7 the model sends |
| `tm11_scalar_allowed_tools` | Live Gemini | A skill written `allowed-tools: lookup_order` is indexed with that tool, and Gemini loads the skill and calls it |
| `tm16_integral_float_arguments` | Scripted OpenAI-compatible server, live OpenAI | A `#[tool]` with a `u32` field runs when the model sends `3.0` |
| `tm17_gemini_dialect` | Live Gemini | A declaration with `true`, a numeric enum, a nullable string, a union and a map is accepted, keeping nullability, the union and the map's value type |
| `ar22_agent_tool_code_execution` | Live Gemini with code execution | The `AgentTool` result carries the delegate's `codeExecutionResult` |
| `tm4_tm13_container_timeout` | Live Gemini, a shell script standing in for `docker` | `ContainerCommandExecutor` names the container, runs it with `--cap-drop=ALL`, `no-new-privileges` and a PID limit, and kills it by name when the timeout passes |
| `tm5_auth_http_timeouts` | Local server that never answers | `JwksCache::refresh`, `OidcProvider::from_discovery` and `TokenExchangeBroker::token_for` each give up on their own |
| `tm12_token_exchange_single_flight` | Local token endpoint | Eight concurrent `token_for` calls for one token send one exchange |

`tm4_tm13_container_timeout` needs no Docker daemon. `tm5` and `tm12` use no live
identity provider or token service.
