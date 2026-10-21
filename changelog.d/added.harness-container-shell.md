- **Container shell for `adk-harness`** (feature `docker`, spec 3.1 §7.9): `ShellIsolation::Container(ContainerConfig)`
  runs `bash` through `ContainerBashTool` in one container per (app, user, session).
  - The container is provisioned through `SandboxClient::provision`/`start` on the session's
    first command, kept across runs, and stopped after `ContainerConfig::idle_ttl` (15 minutes
    by default), on a timeout or a freeze, and by `Harness::close`.
  - The host workspace is bind-mounted at `/workspace` (`DockerClient::with_bind_mount`), so
    `bash` and `read_file` see the same files. Every protected path that exists is mounted
    read-only over it, and a probe inside the container refuses the container if one can be
    written. The network is off by default and commands run as the current `uid:gid`.
  - Commands run through `SandboxSession::exec_command` with the workspace's timeout ceiling
    and a post-hoc output cap, and carry the `bash` annotations, the failure hint, the shell
    ledger entry and the `harness.bash.*` records of the OS sandbox. The unsandboxed retry is
    off unless `allow_unsandboxed_retry(true)`.
  - The Docker daemon is probed at build: `IsolationStatus::Container { image }`, or
    `Degraded` and `harness.sandbox_unavailable` under `fail_if_unavailable`. A container that
    cannot be provisioned refuses the call and never falls back to the host.
  - `auto_allow_sandboxed` runs container commands in Ask without a prompt when the network
    is off and every root-level protected path exists.
  - Under the container shell, hooks with `inherit` isolation and shell credentials are
    refused at build, and a goal's `until` check is refused rather than run on the host.
- **`sandbox-docker` workflow**: runs the `DockerClient` and container-shell tests against a
  Docker daemon on `ubuntu-24.04`.
