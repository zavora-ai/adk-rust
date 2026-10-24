- **The harness OS sandbox runs on Windows** (`adk-harness`, `adk-devtools`, `adk-sandbox`):
  `ShellIsolation::OsSandbox` on Windows now runs `bash` under the restricted-token enforcer
  instead of reporting `Degraded` and running on the host (spec 3.2 §10 W5). The enforcer
  starts `sh -c` itself through `WindowsEnforcer::spawn_contained`, in a Job Object (W1), and
  the devtools `BashTool` keeps its timeout ceiling, output cap, streaming, empty standard
  input, process-tree kill and the freeze's kill around it. The shell is `OsSandbox { enforcer:
  "restricted-token" }` when `WindowsEnforcer::probe_confined` runs a command under the token,
  and `Degraded`, or refused under `fail_if_unavailable`, otherwise. The command writes only
  the workspace and the scratch directory, which are labelled Low, and reads what the user may
  read. The token leaves the network open and its grants stay on the workspace, so on Windows
  the `bash`
  declaration reports network access, Plan mode denies `bash`, every sandboxed command is
  asked in Ask and Auto, `auto_allow_sandboxed` and hooks with `isolation = "inherit"` fail
  the build, and `read_toolchains` grants only toolchain directories in the user's profile.
