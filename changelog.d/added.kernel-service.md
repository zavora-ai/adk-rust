- **`adk-rust up --install`, `--uninstall` and `--print`: the kernel as a service** (`adk-kernel`,
  `adk-cli`; spec 3.2 §7.12, §10 W3): `up --install` keeps `adk-rust up` running as the user —
  a launchd agent on macOS, a systemd user unit on Linux, a Windows service under the user's
  account — writing its output to the owner-only `<home>/kernel.log` and restarting it when it
  fails; `--uninstall` removes it and keeps the home; `--install --print` renders the
  definition without installing. The service runs the typed command line with `--home` and
  `--dir` pinned and refuses secrets (`--api-key`, `--auth-token`, a `--store` URL with a
  password, a non-loopback bind). On Windows it grants the account "Log on as a service",
  checks the password (`ADK_SERVICE_PASSWORD` or a prompt) and restarts after 5, 10 and 30
  seconds. `adk_kernel::service` holds `ServiceDefinition` (rendered for any platform on any
  platform), `install`, `uninstall`, `stop_requested` and the Windows `run_as_service`;
  `Kernel::shutdown_on_signal` also returns on a Windows service stop. The
  `Kernel Service` workflow installs, kills, restarts and removes the service for real on all
  three platforms.
- **The kernel's control plane on a named pipe on Windows** (`adk-kernel`; spec 3.2 §10 W2):
  `ControlPlane` serves `\\.\pipe\adk-rust-kernel-<tag>` (`ControlPlaneConfig::pipe_name`)
  where Unix has the socket, `ControlClient::connect_local` finds it and `ControlClient::pipe`
  reaches it, so `adk-rust ps`, `attach` and the other control commands no longer need
  `--server` on Windows. `ControlTransport::Pipe` names it in `kernel.control` records.
