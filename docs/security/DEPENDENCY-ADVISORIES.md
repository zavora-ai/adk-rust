# Dependency Security Advisories

## Overview

This document tracks known security advisories affecting ADK-Rust transitive dependencies. Each entry includes the advisory identifier, affected crate, severity assessment, impact on ADK-Rust, current status, and disposition (accepted risk, mitigation planned, or resolved).

The purpose of this file is to provide transparency to consumers about the security posture of ADK-Rust's dependency tree and to document informed risk-acceptance decisions where upstream fixes are unavailable or upgrades are deferred.

These accepted advisories are also configured in [`.cargo/audit.toml`](../../.cargo/audit.toml) so that `cargo audit` passes in CI while still surfacing new, unreviewed advisories.

**Last reviewed:** 2026-10-17 (3.0.0 release candidate)

---

## Resolved in 2.0.0

| Advisory | Crate | Resolution |
|---|---|---|
| RUSTSEC-2026-0193, RUSTSEC-2026-0213 | `ammonia` | Lockfile updated to 4.1.4. |
| RUSTSEC-2026-0204 | `crossbeam-epoch` | Lockfile updated to 0.9.20. |
| RUSTSEC-2026-0194, RUSTSEC-2026-0195 | `quick-xml` (declared) | `adk-eval` now requires quick-xml 0.41. The transitive 0.26 copy is covered below. |
| RUSTSEC-2026-0222, RUSTSEC-2026-0223 | `wasmtime` | Lockfile updated from 46.0.1 to 46.0.2. |

## Resolved in 2.1.0

| Advisory | Crate | Resolution |
|---|---|---|
| RUSTSEC-2026-0176, RUSTSEC-2026-0177 | `pyo3` | Monty updated to 0.0.21, which selects jiter 0.16 and PyO3 0.29.2. The old advisory exceptions were removed from both audit policies. |
| RUSTSEC-2026-0258 | `h2` | Updated the HTTP/2 1.x stack to h2 0.4.19, moved Azure Identity to its matching 0.22 SDK generation, and disabled the AWS Secrets Manager legacy Hyper 0.14 TLS feature. The vulnerable h2 0.3 line is no longer in the lockfile. |

## Resolved since 2.1.0

| Advisory | Crate | Resolution |
|---|---|---|
| RUSTSEC-2026-0098, RUSTSEC-2026-0099, RUSTSEC-2026-0104 | `rustls-webpki` | The lockfile no longer contains a `rustls-webpki` release below 0.103.12. Neither `cargo audit` nor `cargo deny` reports the advisories, and the exceptions were removed from both policies. |

## Resolved in 2.3.0

| Advisory | Crate | Resolution |
|---|---|---|
| RUSTSEC-2026-0321, RUSTSEC-2026-0322, RUSTSEC-2026-0323, RUSTSEC-2026-0324, RUSTSEC-2026-0325, RUSTSEC-2026-0326, RUSTSEC-2026-0327 | `wasmtime`, `wasmtime-wasi` | Lockfile updated from 48.0.3 to 48.0.5 (#704). |
| RUSTSEC-2026-0285 | `rustls` | Workspace and example lockfiles updated to 0.23.45 (#687). |

## Resolved since 2.3.0

| Advisory | Crate | Resolution |
|---|---|---|
| RUSTSEC-2021-0157, RUSTSEC-2021-0158 | `ammonia` | Lockfile updated from 4.1.4 to 4.1.8. |
| RUSTSEC-2026-0338, RUSTSEC-2026-0340, RUSTSEC-2026-0343 | `quinn-proto` | Workspace and example lockfiles updated to 0.11.19. |
| RUSTSEC-2026-0346, RUSTSEC-2026-0347, RUSTSEC-2026-0348, RUSTSEC-2026-0349 | `rustls` | Workspace and example lockfiles updated from 0.23.45 to 0.23.46. |
| RUSTSEC-2026-0352 through RUSTSEC-2026-0359, RUSTSEC-2026-0361 through RUSTSEC-2026-0364 | `wasmtime`, `wasmtime-wasi` | Lockfile updated from 48.0.5 to 48.0.6, with the matching Cranelift 0.135.6 crates. Wasmtime 49.0.3 requires Rust 1.96. |
| RUSTSEC-2026-0367, RUSTSEC-2026-0368, RUSTSEC-2026-0369 | `zip` (declared) | The workspace `zip` dependency (`adk-skill`, `adk-cli` tests) now requires zip 9. The transitive zip 0.6, 6, and 7 copies are covered below. |

---

## Active Advisories

---

### RUSTSEC-2026-0235 — rkyv 0.7 archive validation

> Listed in `.cargo/audit.toml` only. `cargo audit` scans the lockfile and reports
> the package; `cargo deny` resolves the feature graph, never reaches it, and
> would flag the exception as `advisory-not-detected`.

- **Crate:** `rkyv` (0.7.46)
- **Severity:** Memory safety when validating malicious archives containing shared pointers
- **ADK Impact:** The package is recorded only through `rust_decimal`'s optional `rkyv` feature. `rust_decimal` is used by SurrealDB, but neither SurrealDB nor any ADK-Rust feature enables that archive integration.
- **Status:** rkyv fixed the issue on its 0.8 line. `rust_decimal` 1.42.1 still declares rkyv 0.7 as its optional compatibility dependency.
- **Disposition:** Accepted risk — dependency feature is unreachable
- **ADK-Specific Context:** `cargo tree --workspace --all-features --target all -e features -i rkyv@0.7.46` has no reachable package. Active rkyv users in the lockfile resolve to 0.8.18.
- **Mitigation:** Keep the exception only until `rust_decimal` moves or removes its rkyv 0.7 compatibility feature. Never enable `rust_decimal/rkyv`; use the current rkyv 0.8 integration directly for archive handling.

---

### RUSTSEC-2026-0194, RUSTSEC-2026-0195 — quick-xml 0.26: parser denial of service

- **Crate:** `quick-xml` (0.26.0)
- **Severity:** High (7.5) — availability only
- **Advisories:**
  - [RUSTSEC-2026-0194](https://rustsec.org/advisories/RUSTSEC-2026-0194) — quadratic run time checking a start tag for duplicate attribute names
  - [RUSTSEC-2026-0195](https://rustsec.org/advisories/RUSTSEC-2026-0195) — unbounded namespace-declaration allocation in `NsReader`
- **ADK Impact:** `adk-rag` → `lancedb` → `lance-testing` → `pprof` → `inferno` → `quick-xml 0.26`, behind the optional `adk-rag/lancedb` feature.
- **Status:** Patched upstream in quick-xml ≥ 0.41. The 0.26 copy cannot be updated from this workspace: `lancedb` declares `lance-testing` as a normal (not dev) dependency with `default = []`, so no feature selection removes the path. It needs a `lancedb` release that moves `lance-testing` to dev-dependencies.
- **Disposition:** Accepted risk — no ADK-reachable path
- **Conditions:** Both advisories require the attacker to control XML fed to the parser.
- **ADK-Specific Context:** `inferno` parses only the flamegraph SVG that `pprof` generates in-process during profiling. No ADK-Rust request, document, or tool input reaches this parser, and the profiler is not invoked by the RAG backend at runtime.
- **Mitigation:** The crates this workspace declares directly are on the patched line — `adk-eval` requires quick-xml 0.41. `deny.toml` bans `quick-xml < 0.41` with `inferno` as the sole permitted wrapper, so a direct regression cannot hide behind this exception.

---

### RUSTSEC-2026-0367, RUSTSEC-2026-0368, RUSTSEC-2026-0369 — zip < 9: unconfined archive extraction

- **Crate:** `zip` (0.6.6, 6.0.0, 7.2.0)
- **Severity:** High (7.3) for RUSTSEC-2026-0367, Critical (9.1) for RUSTSEC-2026-0369; RUSTSEC-2026-0368 is unscored
- **Advisories:**
  - [RUSTSEC-2026-0367](https://rustsec.org/advisories/RUSTSEC-2026-0367) — SUID, SGID, and sticky bits preserved from archive attributes during extraction
  - [RUSTSEC-2026-0368](https://rustsec.org/advisories/RUSTSEC-2026-0368) — symbolic links created with attacker-chosen targets outside the destination
  - [RUSTSEC-2026-0369](https://rustsec.org/advisories/RUSTSEC-2026-0369) — Unix permissions applied through extracted symlinks outside the destination
- **ADK Impact:**

  | zip | Dependency path | Feature | Vulnerable code reached |
  |---|---|---|---|
  | 0.6.6 | `adk-realtime` → `webrtc-sys` 0.3.40 → (build) `webrtc-sys-build` 0.3.18 | `adk-realtime/livekit` | At build time only, on LiveKit's pinned libwebrtc archive |
  | 6.0.0 | `adk-audio` → `ndarray-npy` 0.10.0 (`npz`) | `adk-audio/kokoro` | No — `NpzReader` reads entries by name |
  | 7.2.0 | `adk-audio` → `candle-core` 0.9.2, also via `qwen_tts` 0.1.1 | `adk-audio/qwen3-tts` | No — reads `.npz`/`.pth` entries by name, writes `.npz` |
  | 7.2.0 | `adk-mistralrs` → `mistralrs` 0.8.1 → `candle-core` 0.10.2 | default | No — same `candle-core` code |

- **Status:** Patched upstream in zip ≥ 9.0.0. No release of any wrapper depends on zip 9: every `webrtc-sys-build` release requires zip 0.6, `ndarray-npy` 0.10.0 is the latest release and requires zip 6, and `candle-core` 0.11.0 requires zip 8.6. `qwen_tts` 0.1.1 requires `candle-core` 0.9 and `mistralrs` 0.8.1 requires `candle-core` 0.10.2.
- **Disposition:** Accepted risk — no runtime path; the one build-time extraction reads a vendor archive
- **Conditions:** All three advisories are in `ZipArchive::extract` and `ZipStreamReader::extract`, and require the attacker to control the archive being extracted.
- **ADK-Specific Context:** `ndarray-npy` and `candle-core` never call either extraction API; they open archives with `ZipArchive::new` and read single entries with `by_name`. The `webrtc-sys-build` build script calls `ZipArchive::extract` on the libwebrtc archive it downloads over HTTPS from the `livekit/rust-sdks` GitHub release `webrtc-51ef663`. That archive supplies the native library linked into the binary, so a party able to substitute it already controls linked code; the extraction flaw adds no capability. No ADK-Rust runtime path extracts a zip archive with these versions.
- **Mitigation:** The crates this workspace declares directly are on the patched line — the workspace `zip` dependency requires zip 9, and `adk-skill` validates registry archives in memory without either extraction API. `deny.toml` bans `zip < 9` with `webrtc-sys-build`, `ndarray-npy`, and `candle-core` as the sole permitted wrappers, so a direct regression cannot hide behind this exception. Set `LK_CUSTOM_WEBRTC` to a locally built libwebrtc to skip the download and extraction. Remove the exception when the wrappers move to zip 9.

---

### RUSTSEC-2023-0071 — rsa: Marvin Attack (Timing Side-Channel)

- **Crate:** `rsa`
- **Severity:** Moderate
- **Advisory:** [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071)
- **ADK Impact:** Transitive dependency via `adk-auth` → `azure_security_keyvault` → `rsa`
- **Status:** No upstream fix available. The `rsa` crate maintainers are aware but have not released a patched version.
- **Disposition:** Accepted risk
- **Conditions:** The Marvin attack requires an attacker to perform precise timing measurements of RSA decryption operations. This is only exploitable when:
  1. The application performs RSA PKCS#1 v1.5 decryption (not signing)
  2. The attacker can submit arbitrary ciphertexts and observe decryption timing with high precision
  3. The attacker has local or adjacent network access with minimal latency jitter
- **ADK-Specific Context:** ADK-Rust uses the `rsa` crate transitively through Azure Key Vault operations. The typical usage pattern (key retrieval and signature verification) does not expose the vulnerable decryption path to attacker-controlled inputs.
- **Mitigation:** Monitor upstream for a fix. If consumers use Azure Key Vault RSA decryption with untrusted input, consider using a separate HSM-backed key.

---

### RUSTSEC-2026-0174 — http-types: ASCII invariant in `Authorization` values

- **Crate:** `http-types` (2.12.0)
- **Severity:** Informational (unsoundness notice)
- **Advisory:** [RUSTSEC-2026-0174](https://rustsec.org/advisories/RUSTSEC-2026-0174)
- **ADK Impact:** Transitive dependency via `adk-auth` → `azure_core` → `http-types`.
- **Status:** The crate is unmaintained upstream; the Azure SDK is migrating away from it.
- **Disposition:** Accepted risk — no ADK-reachable path
- **ADK-Specific Context:** ADK-Rust never constructs `http-types` header values. The Azure SDK builds its own `Authorization` headers from tokens it obtains itself.
- **Mitigation:** Listed in `deny.toml` only (cargo-audit treats the notice as informational). Remove the exception when the Azure SDK drops `http-types`.

---

### lru 0.12.5 — Unsoundness

- **Crate:** `lru` (0.12.5)
- **Severity:** Low (memory safety, but requires specific usage patterns)
- **ADK Impact:** Transitive dependency via `adk-rag` → `tantivy`/`lancedb` → `lru`
- **Status:** Upstream issue acknowledged. The unsoundness relates to internal unsafe code that can lead to undefined behavior under specific access patterns.
- **Disposition:** Replacement planned — tracking upstream fix
- **Conditions:** The unsoundness requires specific concurrent access patterns to the LRU cache that are unlikely in ADK-Rust's single-threaded-per-request usage of tantivy/lancedb.
- **ADK-Specific Context:** The `lru` crate is used internally by tantivy and lancedb for segment caching. ADK-Rust does not directly interact with the `lru` API, and the affected code paths require concurrent mutable access patterns that the upstream libraries do not exercise.
- **Mitigation:** Monitor tantivy/lancedb releases for an update that replaces or upgrades `lru`. Consider switching to an alternative RAG backend if a fix is not forthcoming within two minor releases.

---

### rand 0.7.3 — Outdated

- **Crate:** `rand` (0.7.3)
- **Severity:** Informational (no known security vulnerability)
- **ADK Impact:** Transitive dependency via `adk-auth` → `azure_security_keyvault` → `rand` 0.7.3
- **Status:** The `rand` 0.7.x line is outdated (current stable is 0.8.x+). No security advisory exists, but `cargo audit` flags it as unmaintained/outdated.
- **Disposition:** Accepted risk — no security impact
- **Conditions:** N/A. The outdated version of `rand` has no known security vulnerabilities. The flag is purely informational.
- **ADK-Specific Context:** The `rand` 0.7.3 dependency is pulled in by the Azure SDK crate. ADK-Rust does not depend on `rand` 0.7.3 directly. The Azure SDK team controls when to upgrade their dependency.
- **Mitigation:** No immediate action required. Monitor Azure SDK releases for an upgrade to `rand` 0.8.x. This is a cosmetic `cargo audit` warning with no security impact.

---

## Unmaintained Crates

The following crates are flagged by `cargo audit` as unmaintained. Each has been reviewed for security impact and assigned a disposition.

| Crate | Advisory | Version | Dependency Chain | Disposition | Justification |
|-------|----------|---------|-----------------|-------------|---------------|
| `async-std` | [RUSTSEC-2025-0052](https://rustsec.org/advisories/RUSTSEC-2025-0052) | 1.13.2 | `adk-auth` → `azure_security_keyvault_secrets`; `adk-realtime` → `livekit` → `async-tungstenite` | Acceptable — no security impact | Discontinued runtime, used only as transitive async shim. Will be removed when upstream migrates. |
| `atomic-polyfill` | [RUSTSEC-2023-0089](https://rustsec.org/advisories/RUSTSEC-2023-0089) | 1.0.3 | `adk-rag` → `surrealdb` → `geo-types` → `rstar` → `heapless` | Acceptable — no security impact | Polyfill for non-std targets. ADK-Rust uses std only. |
| `audiopus_sys` | [RUSTSEC-2026-0150](https://rustsec.org/advisories/RUSTSEC-2026-0150) | 0.2.2 | `adk-realtime` → `audiopus` | Replacement planned | Tracking replacement with `opus-rs` or direct `libopus-sys`. |
| `backoff` | [RUSTSEC-2025-0012](https://rustsec.org/advisories/RUSTSEC-2025-0012) | 0.4.0 | `adk-session` → `neo4rs`/`firestore` | Acceptable — no security impact | Pure-Rust retry logic. Functional despite unmaintained status. |
| `bincode` | [RUSTSEC-2025-0141](https://rustsec.org/advisories/RUSTSEC-2025-0141) | 2.0.1 | `adk-rag` → `surrealdb` → `surrealmx` | Acceptable — no security impact | Internal to SurrealDB. No direct ADK usage. |
| `fxhash` | [RUSTSEC-2025-0057](https://rustsec.org/advisories/RUSTSEC-2025-0057) | 0.2.1 | `adk-mistralrs` → `mistralrs` → `bm25` | Acceptable — no security impact | Non-crypto hash for BM25 lookup tables. No security context. |
| `instant` | [RUSTSEC-2024-0384](https://rustsec.org/advisories/RUSTSEC-2024-0384) | 0.1.13 | `adk-auth` → `azure_core` → `http-types` → `futures-lite` → `fastrand` | Acceptable — no security impact | Time shim for non-WASM. Delegates to `std::time::Instant` on native. |
| `number_prefix` | [RUSTSEC-2025-0119](https://rustsec.org/advisories/RUSTSEC-2025-0119) | 0.4.0 | `adk-mistralrs` → `mistralrs` → `indicatif`; `adk-audio` → `tokenizers` → `indicatif` | Acceptable — no security impact | Number formatting for progress bars. No unsafe code. |
| `paste` | [RUSTSEC-2024-0436](https://rustsec.org/advisories/RUSTSEC-2024-0436) | 1.0.15 | Multiple crates (adk-mistralrs, adk-audio, adk-browser, adk-eval, adk-rag, adk-code, adk-auth, adk-session) | Acceptable — no security impact | Compile-time macro. No runtime behavior. Widely used across Rust ecosystem. |
| `rustls-pemfile` | [RUSTSEC-2025-0134](https://rustsec.org/advisories/RUSTSEC-2025-0134) | 1.0.4, 2.2.0 | `adk-auth` → `azure_identity`/`oauth2`/`reqwest`; `adk-rag` → `qdrant-client`; `adk-tool` → `google-cloud-spanner` | Replacement planned | Superseded by built-in `rustls` PEM parser. Will be removed as deps update. |

---

## Disposition Legend

| Disposition | Meaning |
|---|---|
| **Accepted risk** | Vulnerability exists but exploitability conditions make it low-impact for ADK-Rust's usage patterns. No immediate action planned. |
| **Replacement planned** | A fix or replacement is expected upstream. ADK-Rust will upgrade when available. |
| **Upgrade deferred** | A fix exists but cannot be applied yet due to compatibility constraints. Scheduled for a future release. |
| **Resolved** | Advisory has been addressed. Entry retained for audit trail. |

---

## Non-OSI dependency licenses

Two optional backends link code under licenses that are not OSI-approved. They are
allowed per-crate in [`deny.toml`](../../deny.toml) rather than through the global
allow-list, so enabling the feature is a visible decision.

| Crate | License | Reached through | Note |
|---|---|---|---|
| `surrealdb`, `surrealdb-core`, `surrealdb-types`, `surrealdb-types-derive`, `surrealdb-strand`, `surrealdb-collections`, `surrealdb-protocol` | Business Source License 1.1 | `adk-rag/surrealdb` | BSL restricts production use of the licensed work until its change date. Review before enabling the SurrealDB vector backend in a commercial deployment. |
| `intel-mkl-src` | Intel Simplified Software License | `adk-mistralrs/mkl` | Intel's redistribution terms apply to the bundled MKL binaries. |
| `inferno` | CDDL-1.0 (OSI-approved, file-level copyleft) | `adk-rag/lancedb` → `lance-testing` → `pprof` | No obligation on this workspace's own sources. |

Neither `surrealdb` nor `intel-mkl-src` is enabled by any default, `standard`,
`enterprise`, or `full` feature tier.

---

## Review Cadence

This document is reviewed at each minor release. Run both supply-chain gates:

```bash
cargo audit
cargo deny check
```

`cargo audit` scans `Cargo.lock`; `cargo deny check` additionally enforces the
license allow-list, the registry allow-list, and the `quick-xml` and `zip` version
floors.
The accepted-advisory lists in [`.cargo/audit.toml`](../../.cargo/audit.toml) and
[`deny.toml`](../../deny.toml) are kept in sync — add the rationale here first.
