- **`bash` stops every process a command starts on Windows** (`adk-devtools`, spec 3.2 §10
  W1): the process-group kill that ends a command's descendants did nothing off Unix, so a
  background process outlived its call, its timeout and a cancelled run. The command now starts
  suspended and runs only once it is in a Job Object that kills its processes when its last
  handle closes; the job is terminated when the shell exits (unless
  `Workspace::bash_kill_group_on_exit(false)`), on a timeout and when the call is dropped, and
  with the agent process. `BashTool` sets the command's creation flags on Windows, replacing
  any a `CommandLauncher` set.
