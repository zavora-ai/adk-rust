- **Harness shell network allowlist** (`adk-harness`, `adk-devtools`): `OsSandboxConfig::egress`
  and `[network] allow = [...]` in `.adk/harness.toml` give sandboxed commands `Proxied` network
  access through one egress proxy per command, narrowed by the run's egress policy (which
  applies alone when the shell sets none). `bash` declares `Enforced` for sandboxed commands
  and nothing for the unsandboxed retry or a host shell; `auto_allow_sandboxed` and the
  unsandboxed retry are off under the shell's policy; the profile digest covers it.
  `CommandLauncher::guarded_command` keeps what a command needs, such as its proxy, until its
  process ends, and `CommandLauncher::egress` gives `bash`'s declaration. Refs #1281.
