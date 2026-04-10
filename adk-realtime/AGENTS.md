# Realtime Concurrency and Audio Rules

## Context

This repository contains latency-sensitive realtime voice paths built on:

- async session orchestration in `adk-realtime`
- LiveKit / WebRTC audio bridging
- bidirectional WebSocket transports (e.g. Gemini)
- short, per-frame audio hot paths

The real risks are **not** compile errors. They are:
- lock contention in async code
- holding lock guards across `.await`
- websocket write-path split-brain
- excessive buffering and allocator churn in audio paths
- unsafe assumptions around lightweight FFI boundaries

**Core directive**: When changing this code, you MUST optimize for low lock contention, short critical sections, and clear ownership of I/O resources.

---

## 1. Choose the mutex by runtime behavior, not style

### `tokio::sync::{Mutex, RwLock}`
**ONLY** for async orchestration state or true cross-task coordination.

Use for: session/config/runner state, async control-plane data.

**Rules**:
- You MUST NEVER use Tokio locks for short CPU-bound hot paths.
- You MUST NEVER keep a Tokio lock guard alive across network/provider `.await` calls.
- You MUST prefer the `session_handle()` pattern in `RealtimeRunner`: acquire → clone handle → drop guard → await.

### `parking_lot::Mutex`
For **short, synchronous, CPU-bound hot paths** that NEVER cross `.await`.

Good fits: Opus encoder, short audio-buffer mutations, high-frequency sync sections.

**Rules**:
- You MUST keep the locked scope tiny.
- The guard MUST be dropped before any `.await`.
- It MUST NEVER be used as a substitute for async coordination.

### `std::sync::Mutex`
Default choice for small internal synchronous state that NEVER crosses `.await`.

Good fits: local bookkeeping, small shared counters/flags.

**Rules**:
- You MUST ALWAYS release the guard before any `.await`.
- You MUST ONLY upgrade to Tokio mutex if the data genuinely needs async lifetime.

---

## 2. Never hold session locks across `.await` (MANDATORY)

In `RealtimeRunner` and helpers (`send_audio`, `send_text`, `commit_audio`, `create_response`, `interrupt`, `next_event`, etc.):

**Bad**:
```rust
let session = self.session.read().await;
session.next_event().await;        // ← lock still held
```
