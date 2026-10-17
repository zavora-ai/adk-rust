- **zip 9 and patched lockfiles** (`adk-skill`, workspace and every standalone example):
  the workspace `zip` dependency moves from 8.6 to 9.0, which resolves RUSTSEC-2026-0367,
  RUSTSEC-2026-0368, and RUSTSEC-2026-0369 for `adk-skill` (feature
  `vertex-skill-registry`). The workspace lockfile moves to ammonia 4.1.8, quinn-proto
  0.11.19, rustls 0.23.46, and wasmtime 48.0.6 (`adk-sandbox`, feature `wasm`); the
  example lockfiles move to quinn-proto 0.11.19 and rustls 0.23.46. No API change. The
  zip 0.6, 6, and 7 copies that `webrtc-sys-build`, `ndarray-npy`, and `candle-core`
  require have no release on zip 9 and are recorded in
  `docs/security/DEPENDENCY-ADVISORIES.md`.
