- **zip 9** (`adk-skill`): the workspace `zip` dependency moves from 8.6 to 9.0, so
  `adk-skill` with `vertex-skill-registry` no longer pulls a zip release affected by
  RUSTSEC-2026-0367, RUSTSEC-2026-0368, and RUSTSEC-2026-0369 (unconfined archive
  extraction). `adk-skill` reads registry archives in memory without the affected extraction
  APIs. No API change. The zip 0.6, 6, and 7 copies that `webrtc-sys-build`, `ndarray-npy`,
  and `candle-core` require have no release on zip 9 and are recorded in
  `docs/security/DEPENDENCY-ADVISORIES.md`.
- **Patched lockfiles** (workspace and standalone examples): the workspace `Cargo.lock` moves
  to ammonia 4.1.8, hickory-proto, hickory-resolver, and hickory-net 0.26.3, quinn-proto
  0.11.19, rustls 0.23.46, and wasmtime 48.0.6, and most standalone example lockfiles to
  quinn-proto 0.11.19 and rustls 0.23.46. Lockfiles affect only builds of this repository and
  its examples; published crates keep their version requirements, so a downstream project
  gets these fixes by updating its own lockfile.
