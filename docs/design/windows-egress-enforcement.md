# Windows egress enforcement — design note

| | |
|---|---|
| **Status** | Design note for review, 2026-11-01; not implemented |
| **Issue** | #1281, decision 2 |
| **Today** | The restricted-token enforcer (`WindowsEnforcer`) has no network filter. It refuses `NetworkAccess::Denied` and `Proxied` with `SandboxError::Unenforceable` unless the caller builds it with `acknowledge_open_network()`, and then runs the command with the network open |

## 1. The gap

On macOS, Seatbelt admits one outbound destination, the egress proxy's loopback port. On
Linux, bubblewrap's empty network namespace holds the proxy's listener. A Windows process run
under a restricted, Low-integrity token keeps every network path of the user: there is no
per-process network filter in the token model. A `Proxied` or `Denied` policy therefore cannot
be honoured, and the harness shell and `ExternalGenerator` acknowledge the open network.

## 2. Options

| | Windows Filtering Platform (WFP) | AppContainer |
|---|---|---|
| Mechanism | Persistent or dynamic filters at the ALE connect layers (`FWPM_LAYER_ALE_AUTH_CONNECT_V4/V6`) matching the confined process's app id or a dedicated SID, permitting only `127.0.0.1:<proxy port>` | The process runs in an AppContainer without the `internetClient`, `internetClientServer` and `privateNetworkClientServer` capabilities; loopback is blocked for AppContainers unless exempted |
| Privilege | Adding filters needs administrator rights: an elevated, installed service owns them, and the agent asks it per call | None to create the container; a loopback exemption for the proxy port needs administrator rights (`CheckNetIsolation LoopbackExempt`, or a firewall rule), once per container profile |
| Match per call | A filter per call keyed on the process's token SID (a per-call restricting SID) or app id; removed when the call ends | One container profile per call or per harness, with its own SID |
| Compatibility | Any program; the existing restricted token stays | Programs that expect the user's profile, named objects and MSYS2 namespaces need grants to the container SID; Git for Windows' MSYS2 runtime is the known hard case the restricted token already works around |
| Failure mode | Service absent or filter not added: refuse the call | Exemption absent: the process reaches nothing (fail closed) |

## 3. Recommendation

1. Ship a **WFP tier** behind an opt-in Windows service (`adk-sandbox` feature, e.g.
   `sandbox-windows-wfp`): the agent asks the service to add per-call filters for a fresh
   restricting SID in the call's token, permitting only the proxy's loopback port and, for
   `Denied`, nothing. The enforcer reports `NetworkSupport { denied: true, proxied: true }` only
   when the service answers its probe, and refuses otherwise, as today.
2. Keep AppContainer as a later, unprivileged tier for programs that run in it unchanged; its
   loopback exemption is the one privileged step.
3. Until then, `acknowledge_open_network()` stays the explicit, logged opt-in.

## 4. Open questions

- Who installs and updates the WFP service, and how its socket is authenticated to the agent.
- Whether a per-call SID in the restricted token survives the MSYS2 runtime's own child
  processes (they inherit the token, so they should).
- Filter clean-up after a crash: dynamic sessions remove filters when the service exits, which
  leaves calls open only while the service is down, and those calls are refused by the probe.
- The CI story: GitHub's Windows runners allow WFP changes from an elevated job; a merge-tier
  job would prove the tier live.
