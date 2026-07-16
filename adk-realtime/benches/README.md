# Realtime audio payload ownership benchmarks

These benchmarks compare benchmark-local `Vec<u8>` and `bytes::Bytes` audio chunks before any production or public API change is proposed.

## Scope

The matrix uses realistic 20 ms frames:

| Format | Bytes |
|---|---:|
| G.711 mu-law, 8 kHz mono | 160 |
| PCM16, 16 kHz mono | 640 |
| PCM16, 24 kHz mono | 960 |

`audio_payload_ownership` measures owned and borrowed construction, one clone, and four-way fan-out with Criterion. `audio_payload_tail_latency` measures bounded Tokio task handoff and four-task fan-out. `audio_payload_allocations` reports allocation operations, allocated bytes, and live heap while fan-out clones are held.

Provider calls, sockets, codecs, resampling, Base64 encoding, and network latency are intentionally excluded. The results cannot support claims about complete provider runtime performance.

## Reproduction

Run from the repository root through the reproducible development shell:

```bash
devenv shell cargo bench -p adk-realtime --no-default-features --bench audio_payload_ownership
devenv shell cargo bench -p adk-realtime --no-default-features --bench audio_payload_tail_latency
devenv shell cargo bench -p adk-realtime --no-default-features --bench audio_payload_allocations
```

Before recording results, capture the environment:

```bash
rustc -Vv
cargo -V
uname -a
lscpu
```

Use an otherwise idle machine and the default optimized bench profile. Run the complete matrix three times without changing the environment, and report the median result for each metric. Do not commit generated Criterion output under `target/`.

## Interpretation

An internal `Bytes` follow-up is justified only when four-way fan-out reduces p95 latency by at least 15% on two of the three frame sizes, reduces allocated bytes by at least 50%, and does not regress move-only p95 latency by more than 5%. Any public `AudioChunk` storage change remains a separate Semver-reviewed proposal.
