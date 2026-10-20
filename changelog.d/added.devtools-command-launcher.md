- **`CommandLauncher` for `bash`** (`adk-devtools`): `BashTool::with_launcher` takes an
  `Arc<dyn CommandLauncher>` that builds the process for each script — program, arguments,
  working directory and environment — and names its isolation, so a sandbox can wrap the
  command while the tool keeps its timeout ceiling, output cap, streaming and process-group
  kill. `HostLauncher` (`sh -c` in the workspace root with the workspace environment) is the
  default and reproduces the previous spawn. Spec 3.1 §10 D1.
