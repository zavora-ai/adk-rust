- **Auto mode runs sandboxed commands without approval on macOS** (`adk-harness`): Seatbelt
  now refuses writes, by name, to a root-level protected path that does not exist or is a
  symbolic link, so a command can neither create `.envrc` or `lefthook.yml` nor replace a
  `CLAUDE.md` link, and `--mode auto --sandbox` and `auto_allow_sandboxed` no longer need all
  twelve protected paths to exist. bubblewrap and the container shell, which bind-mount
  paths, keep that requirement.
