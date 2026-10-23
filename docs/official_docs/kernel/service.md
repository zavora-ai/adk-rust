# Running the Kernel as a Service

`adk-rust up --install` hands the [agent kernel](index.md) to the platform's service manager,
which starts `adk-rust up` as you, writes its output to the kernel home, and restarts it when
it fails (spec 3.2 §7.12). `adk-rust up --uninstall` reverses it. Without either flag the
kernel is a plain foreground process, which is what containers and the managed runtime use.

```bash
adk-rust up --mode ask --dir ~/project --install     # install and start
adk-rust ps                                          # reach it as before
adk-rust up --uninstall                              # stop and remove it
```

```text
$ adk-rust up --mode ask --dir ~/project --install
Installed the kernel as a launchd agent: adk-rust-kernel-7c1e04b9d2a35f80
  definition /Users/ana/Library/LaunchAgents/adk-rust-kernel-7c1e04b9d2a35f80.plist
  runs /Users/ana/.cargo/bin/adk-rust up --mode ask --home "/Users/ana/Library/Application Support/adk-rust" --dir /Users/ana/project
  home /Users/ana/Library/Application Support/adk-rust
  log  /Users/ana/Library/Application Support/adk-rust/kernel.log
  note: it starts again at each login; `launchctl print gui/501/adk-rust-kernel-7c1e04b9d2a35f80` shows it
The kernel answers on unix:/Users/ana/Library/Application Support/adk-rust/kernel.sock.
Reach it with `adk-rust ps`; remove it with `adk-rust up --uninstall`.
```

## What each platform gets

Each form runs as you, so a store key held in the OS keychain (`--store-key keychain:<entry>`)
and a provider key saved by first-run setup stay reachable.

| Platform | Form | Definition | Starts | Restarts |
|---|---|---|---|---|
| macOS | launchd agent, in your GUI session (the user domain over SSH) | `~/Library/LaunchAgents/<name>.plist` | At login | `KeepAlive` on an unsuccessful exit, after 10 s |
| Linux | systemd user unit | `<config dir>/systemd/user/<name>.service` | With your user manager: at login, or at boot after `loginctl enable-linger` | `Restart=on-failure`, after 5 s |
| Windows | Windows service under your account | The service control manager | At boot (delayed automatic start) | Failure actions: after 5 s, 10 s, 30 s; an exit with an error counts |

| Behaviour | All platforms |
|---|---|
| Service name | `adk-rust-kernel-<tag>`, the tag derived from the kernel home, so each home has its own service |
| Output | `<home>/kernel.log`, created owner-only (`0600`, or an owner-only DACL on Windows), appended to |
| Stop | `SIGTERM` (launchd, systemd) or the service control manager's stop: the kernel drains for up to 30 s, then stops; a drained stop exits 0 and is not restarted |
| Reinstall | `--install` again replaces the service of the same home and restarts it |
| Uninstall | Stops and removes the service; the kernel home, its stores and its log stay |

## What the service runs

The service runs the `up` command line you typed, without the installing flags:

| On the command line | In the service |
|---|---|
| `--install`, `--uninstall`, `--print` | Removed |
| `--home DIR` (or `[kernel] home`, or the default) | `--home` with the absolute path |
| `--dir DIR` (or the current directory) | `--dir` with the absolute path, also the working directory |
| Every other flag, such as `--mode`, `--provider`, `--model`, `--bind`, `--store` | Passed as typed |
| `PATH` | Set from your shell, so the tools the agent runs resolve as they do there |

A service definition is readable by others — on Windows every local user can read a
service's command line — so a secret never enters one:

| Refused with `--install` | Instead |
|---|---|
| `--api-key` | Keep the key in the OS keychain, where first-run setup saves it |
| `--auth-token`, or a `--bind` beyond loopback | Serve the installed kernel on loopback; put an authenticating proxy in front, or run `adk-rust up --bind … --auth-token …` yourself |
| A `--store` URL with a password | A URL without one and the backend's own credentials, such as `~/.pgpass` |

Relative paths in other flags resolve against the workspace (`--dir`).

## Printing the definition

`--install --print` renders what `--install` would register and installs nothing:

```bash
adk-rust up --mode ask --install --print > adk-rust-kernel.service
```

```ini
[Unit]
Description=ADK-Rust agent kernel (/home/ana/.config/adk-rust)

[Service]
Type=simple
ExecStart=/home/ana/.cargo/bin/adk-rust up --mode ask --home /home/ana/.config/adk-rust --dir /home/ana/project
WorkingDirectory=/home/ana/project
Environment="PATH=/home/ana/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
UMask=0077
Restart=on-failure
RestartSec=5
TimeoutStopSec=60
StandardOutput=append:/home/ana/.config/adk-rust/kernel.log
StandardError=inherit

[Install]
WantedBy=default.target
```

On macOS it prints the launchd plist; on Windows, a description of the service, its command
line, account, start mode and failure actions.

## Windows

| Requirement | Detail |
|---|---|
| An elevated shell | Registering and removing a service needs one (Run as administrator); the service itself runs as you |
| Your password | The service control manager logs the service on as you with it: set `ADK_SERVICE_PASSWORD`, or type it at the prompt (no echo). adk-rust does not store it; after a password change, run `--install` again |
| "Log on as a service" | `--install` grants it to your account, which a service under a user account needs, and checks the password before registering |

```powershell
adk-rust up --mode ask --dir C:\Users\ana\project --install
Get-Service adk-rust-kernel-*
adk-rust ps
adk-rust up --uninstall
```

The control commands reach the kernel over its named pipe, so `adk-rust ps`, `attach` and the
rest need no `--server` on Windows; see [the control plane](control-plane.md).

## From code

```rust
use adk_kernel::service::{self, ServiceDefinition, ServicePlatform};

fn main() -> adk_kernel::Result<()> {
    let home = std::path::Path::new("/srv/kernel");
    let definition = ServiceDefinition::new("/usr/local/bin/adk-rust", home)
        .with_args(["up", "--home", "/srv/kernel", "--mode", "ask"])
        .with_working_directory("/srv/project")
        .with_env("PATH", "/usr/local/bin:/usr/bin:/bin");
    if let Some(platform) = ServicePlatform::current() {
        println!("{}", definition.render(platform));
    }
    // Installs for real; the password is the Windows account's and is ignored elsewhere.
    let installed = service::install(&definition, None)?;
    println!("installed {} (log: {})", installed.name, installed.log.display());
    // service::uninstall(&ServiceDefinition::name_for(home)) removes it again.
    Ok(())
}
```

`service::run_as_service` is the Windows service's entry point: `adk-rust up` calls it when
the service control manager starts it, and `Kernel::shutdown_on_signal` returns on the
service's stop.

## How it is tested

| Check | Where |
|---|---|
| Each platform's definition rendered against golden output, on every platform; `plutil -lint` accepts the plist | `adk-kernel` unit tests, the PR tier on Linux, macOS (compile) and Windows |
| `up --install --print` from the binary; secrets and a non-loopback bind refused | `adk-cli/tests/kernel_service_cli.rs` |
| A real install, `kill -9` and a restart through the service manager with the process table intact, then `--uninstall` | `.github/workflows/kernel-service.yml`: a systemd user unit on Linux, a launchd agent on macOS, a Windows service on Windows |
