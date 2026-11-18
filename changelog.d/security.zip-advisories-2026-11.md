- **zip 6 leaves the dependency tree** (`adk-audio`, feature `kokoro`): `ndarray-npy` moves
  to 0.11, which reads `.npz` voice files with zip 9, so the `kokoro` feature no longer
  compiles zip 6 and its RUSTSEC-2026-0545, -0547 and -0548 code. The workspace `zip`
  requirement rises to 9.0.5, the first release with RUSTSEC-2026-0548's fix. The zip 7.4.0
  (`candle-core`) and 0.6.6 (`webrtc-sys-build`, build time) copies have no release on zip 9
  and are recorded as accepted in `docs/security/DEPENDENCY-ADVISORIES.md`: neither enables
  the `aes-crypto` or `legacy-zip` features, and neither extracts or streams an archive at
  run time.
