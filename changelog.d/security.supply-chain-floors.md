- **Patched lockfile and version floors** (workspace and standalone examples): the workspace
  `Cargo.lock` moves to rmcp 3.10 (RUSTSEC-2026-0457, RUSTSEC-2026-0461, RUSTSEC-2026-0462),
  wasmtime 48.0.8 (RUSTSEC-2026-0420, RUSTSEC-2026-0422 to RUSTSEC-2026-0424), pulldown-cmark
  0.13.6 (RUSTSEC-2026-0414, RUSTSEC-2026-0433), zstd 0.13.5 (RUSTSEC-2026-0391,
  RUSTSEC-2026-0398, RUSTSEC-2026-0403, RUSTSEC-2026-0410), and quinn-udp 0.5.16
  (RUSTSEC-2026-0538). Published manifests now require the fixed releases, so a downstream
  lockfile cannot select a vulnerable one: `rmcp` 3.9 (`adk-tool`, `adk-acp`,
  `adk-computer-use`), `wasmtime` and `wasmtime-wasi` 48.0.8 (`adk-sandbox`), `pulldown-cmark`
  0.13.6 (`adk-site`, `adk-surface`), and `tokio` 1.53.3 (RUSTSEC-2026-0386, unsound
  `AsyncFd` constructors). Standalone example lockfiles move to rmcp 3.10, tokio 1.54.1, and
  pulldown-cmark 0.13.6 where they reach those crates. libsqlite3-sys 0.37
  (RUSTSEC-2026-0459), crypto-bigint 0.5.5 (RUSTSEC-2026-0387), ocipkg 0.2.9
  (RUSTSEC-2026-0450), and zip 6 and 7 (RUSTSEC-2026-0429) have no fixed release their
  dependents accept; each is recorded with its exposure and removal condition in
  `docs/security/DEPENDENCY-ADVISORIES.md`.
