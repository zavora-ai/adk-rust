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

## Decision criteria

An internal `Bytes` follow-up is justified only when four-way fan-out reduces p95 latency by at least 15% on two of the three frame sizes, reduces allocated bytes by at least 50%, and does not regress move-only p95 latency by more than 5%. Any public `AudioChunk` storage change remains a separate Semver-reviewed proposal.

## Recorded discovery

The matrix was run three times on July 16, 2026. The tables report the median of the three optimized runs. A negative change means `Bytes` was faster than `Vec`; a positive change means it was slower.

Environment:

- Linux `michael-MacPro5-1`, kernel `7.1.3-x64v2-xanmod1`
- x86_64, dual Intel Xeon E5520 at 2.27 GHz, 16 logical CPUs
- rustc 1.94.0 with LLVM 21.1.8
- cargo 1.94.0
- default optimized benchmark profile

### Synchronous ownership cost

| Scenario | Frame | `Vec` | `Bytes` | Change |
|---|---|---:|---:|---:|
| owned | 160 B | 13.578 ns | 12.247 ns | -9.8% |
| owned | 640 B | 14.606 ns | 13.222 ns | -9.5% |
| owned | 960 B | 15.085 ns | 13.860 ns | -8.1% |
| borrowed | 160 B | 32.098 ns | 41.140 ns | +28.2% |
| borrowed | 640 B | 43.946 ns | 52.373 ns | +19.2% |
| borrowed | 960 B | 52.096 ns | 63.078 ns | +21.1% |
| clone once | 160 B | 30.515 ns | 27.917 ns | -8.5% |
| clone once | 640 B | 42.170 ns | 27.965 ns | -33.7% |
| clone once | 960 B | 52.560 ns | 28.438 ns | -45.9% |
| fan-out four | 160 B | 137.90 ns | 174.38 ns | +26.5% |
| fan-out four | 640 B | 189.49 ns | 173.28 ns | -8.6% |
| fan-out four | 960 B | 228.36 ns | 177.62 ns | -22.2% |

### Cross-task p95 latency

| Scenario | Frame | `Vec` p95 | `Bytes` p95 | Change |
|---|---|---:|---:|---:|
| move | 160 B | 1,523 ns | 1,183 ns | -22.3% |
| move | 640 B | 1,057 ns | 1,114 ns | +5.4% |
| move | 960 B | 1,147 ns | 1,139 ns | -0.7% |
| fan-out four | 160 B | 2,828 ns | 2,778 ns | -1.8% |
| fan-out four | 640 B | 3,726 ns | 2,621 ns | -29.7% |
| fan-out four | 960 B | 3,640 ns | 2,648 ns | -27.3% |

The 160 B move result was noisy across runs. The 640 B move median regressed by 5.4%, narrowly exceeding the 5% decision limit.

### Allocation behavior

The allocation results were stable across all three runs:

| Scenario | `Vec` | Warmed `Bytes` |
|---|---|---|
| owned or borrowed | 1 allocation, full frame bytes | 1 allocation, full frame bytes |
| clone once | 1 allocation, full frame bytes | 0 allocations, 0 payload bytes |
| fan-out four | 4 allocations, four full payload copies | 0 allocations, 0 payload bytes |

While four fan-out clones were retained, `Vec` kept 640, 2,560, and 3,840 bytes live for the three frame sizes. Warmed `Bytes` retained one 24-byte promotion/control allocation for each frame size.

### Decision

The results do not justify changing the public `AudioChunk.data` field from `Vec<u8>` to `Bytes`.

`Bytes` clearly reduces clone and fan-out allocation pressure and improves larger PCM fan-out timing and tail latency. It is slower when copying borrowed input, slower for synchronous four-way fan-out of the 160 B G.711 frame, and exceeds the move-only p95 regression limit for the 640 B frame.

Keep `Vec<u8>` at the public boundary. A future optimization should introduce `Bytes` only at a production-profiled shared or fan-out boundary for larger PCM frames, then include conversion cost in a production-path benchmark.

These measurements isolate ownership operations. They do not measure codec work, Base64 encoding, provider or network latency, end-to-end call latency, or whole-process CPU utilization, and should not be generalized beyond the recorded environment without another run.
