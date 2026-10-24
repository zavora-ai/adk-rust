- **`adk_sandbox::process_tree::JobObject`** (`adk-sandbox`, feature `process-tree`, implied
  by `sandbox-windows`; Windows only): a Job Object that holds a process tree and ends it on
  `terminate()` or when its last handle closes (spec 3.2 §10 W1). `contain_suspended(pid)`
  places a process started with `CREATE_SUSPENDED` in a new job before it runs. The devtools
  `bash` tool, `McpServerManager` and `WindowsEnforcer::run_confined` share it; `adk-devtools`
  depends on `adk-sandbox` on Windows for it.
