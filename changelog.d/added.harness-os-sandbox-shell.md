- **Sandboxed `bash` in `adk-harness`** (spec 3.1 §7.9): `ShellIsolation::OsSandbox` now
  runs commands under Seatbelt or bubblewrap through `SandboxLauncher`, a devtools
  `CommandLauncher` over `adk_sandbox::sandbox::sandboxed_command`, and the host shell runs
  through `HostLauncher`.
  - The workspace is read-write with its protected paths (`.git`, `AGENTS.md`, `.envrc`, …)
    read-only, or read-only in Plan and ReadOnly; `HOME` and `TMPDIR` are a per-session
    scratch directory; agent sockets and credential variables never pass in; the network is
    off by default; toolchains stay readable, including the macOS developer directory and
    merged-`/usr` symlinks on bubblewrap.
  - The enforcer is probed at build and on every call: `IsolationStatus::Degraded` (host
    behind approvals) or `harness.sandbox_unavailable` under `fail_if_unavailable`, and a
    sandbox lost after build refuses the call instead of running it on the host.
  - A failed sandboxed command reports `sandbox: { enforcer, denialSuspected, hint }`; an
    unsandboxed retry names the failed call, needs a non-waivable approval and is consumed
    once, with `harness.bash.retry_unsandboxed` records from the policy and the tool.
  - `auto_allow_sandboxed` turns on only when the enforcer passes a nested read-only
    conformance probe, the network is off and every root-level protected path exists;
    otherwise `build()` fails with `harness.auto_allow_sandboxed_unavailable` naming them.
  - The `bash` annotations carry the sandbox profile digest, so standing rules and held
    calls never carry across sandbox configurations.
- **`harness_redteam` example**: scripted attacker scenarios against the 3.0 `CodingAgent`,
  the harness without a sandbox and the harness under the platform enforcer, and live
  sandboxed-shell scenarios on OpenAI, Anthropic and Gemini.
