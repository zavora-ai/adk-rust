- **Stale rustdoc corrected** (`adk-tool`, `adk-code`): `McpToolset::with_task_support`
  described per-tool task support, which SEP-2663 removed; it now states that the server
  decides per call and that tasks need `AdkClientHandler::with_tasks`, and its example
  compiles. `adk_code::compat` claimed the `CodeExecutor` layer was deprecated in 0.5 and
  removed in 0.6; it now describes the two supported execution layers. The `memory-tools`
  install snippet named version 0.8.
