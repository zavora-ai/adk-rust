# Realtime Concurrency and Audio Rules

## Context

This repository contains latency-sensitive realtime voice paths built on:

- async session orchestration in `adk-realtime`
- LiveKit / WebRTC audio bridging
- bidirectional WebSocket transports such as Gemini
- short, per-frame audio hot paths

You MUST optimize for low lock contention, short critical sections, and clear ownership of I/O resources.

---

## 1. Choose the mutex by runtime behavior

### `tokio::sync::{Mutex, RwLock}`
Use for async orchestration state and true cross-task async coordination.

- You MUST NOT use Tokio locks for short CPU-bound hot paths.
- You MUST NOT keep Tokio lock guards alive across provider or network `.await` calls when a handle can be cloned first.
- You MUST prefer the `session_handle()` pattern in `RealtimeRunner`: lock → clone → drop guard → await.

**References**
- Tokio documents that the async mutex is **more expensive** than the blocking mutex and that the main use case is the ability to keep the guard across `.await`; for plain data, `std::sync::Mutex` is often preferred, and `parking_lot` is also called out as a good fit. [^tokio-mutex]
- Tokio `RwLock` is appropriate for async shared state coordination, but the same “do not hold guards longer than necessary” principle still applies. [^tokio-rwlock]

### `parking_lot::Mutex`
Use for short, synchronous, CPU-bound hot paths that never cross `.await`.

Good fits:
- Opus encoder access
- short audio-buffer mutation paths
- other high-frequency sync-only sections

Rules:
- You MUST keep the locked scope tiny.
- You MUST drop the guard before any `.await`.
- You MUST NOT use it as a substitute for async orchestration locks.

**References**
- `parking_lot::Mutex` is a blocking mutex with **eventual fairness** and **no poisoning**; it is therefore a good fit for short sync critical sections, not for async state that must survive across `.await`. [^parking-lot-mutex]

### `std::sync::Mutex`
Use as the default sync mutex for small internal state that does not cross `.await`.

Good fits:
- local bookkeeping
- small shared flags, counters, or state
- places where poisoning is acceptable

Rules:
- You MUST drop the guard before any `.await`.
- You MUST ONLY switch to Tokio mutexes when async lifetime is genuinely required.

**References**
- The standard mutex is the baseline blocking mutex and includes poisoning semantics after panic. [^std-mutex]
- Tokio explicitly says the blocking mutex is often preferred for plain data. [^tokio-mutex]

---

## 2. Never hold session locks across `.await`

In `RealtimeRunner` helpers such as `send_audio`, `send_text`, `commit_audio`, `create_response`, `interrupt`, and `next_event`:

- acquire the lock
- clone the session handle
- drop the guard
- then perform the async call

You MUST NOT await provider I/O while still holding a session lock guard.

**References**
- This follows Tokio’s guidance that async mutexes are primarily for cases where the guard must cross `.await`; if you do not need that, prefer shorter lock lifetimes and cheaper blocking primitives where possible. [^tokio-mutex]

---

## 3. Use a single writer for WebSocket sinks

For bidirectional WebSocket sessions:

- one dedicated `writer_task` MUST own the sink
- all outbound messages MUST go through a bounded `tokio::sync::mpsc`
- `close()` MUST send `Message::Close(...)` through that channel and await writer shutdown

You MUST NOT allow multiple methods to write directly to the sink through a shared mutex.

**References**
- **PROJECT RULE:** this is an architectural rule for this repository, not a literal sentence from one upstream doc.
- It is informed by the `Sink` model, which requires mutable access to send items, and by Tokio’s bounded `mpsc` model for coordinated async message passing and backpressure. [^futures-sink] [^tokio-mpsc]

---

## 4. Treat audio paths as hot paths

- You MUST keep critical sections short.
- You MUST extract buffered data under a short sync lock, then perform async work after the lock is released.
- You SHOULD prefer `bytes::Bytes` / `bytes::BytesMut` in high-frequency buffering paths when they reduce copies or reallocations.
- You MUST avoid casual `Vec<u8>` use in the hottest paths when `Bytes` would be a better fit.

**References**
- `Bytes` is designed for cheap cloning and shared byte storage; `BytesMut` is the mutable companion for efficient incremental buffer building. [^bytes] [^bytesmut]
- **PROJECT RULE:** “avoid casual `Vec<u8>` in the hottest paths” is a repo policy derived from realtime latency goals, not a blanket prohibition from the `bytes` crate docs.

---

## 5. Do not add `block_in_place` around lightweight LiveKit FFI by default

You MUST NOT wrap lightweight calls such as `NativeAudioStream::new(...)` in `tokio::task::block_in_place(...)` unless profiling proves they are meaningful blockers.

Why:
- it is risky on `current_thread` runtimes
- it increases test/runtime fragility
- it is not the default fix for FFI boundaries

If isolation is required and lifetimes allow it, you SHOULD prefer `spawn_blocking`.

**References**
- Tokio documents that `block_in_place` cannot be used on the `current_thread` runtime and is intended for blocking operations that cannot be avoided. [^tokio-block-in-place]
- Tokio documents `spawn_blocking` as the standard mechanism for offloading blocking work. [^tokio-spawn-blocking]

---

## 6. Keep buffering conversational unless measurements justify otherwise

You MUST use low-latency buffering for interactive voice paths.

You MUST NOT increase buffering substantially unless profiling shows it is necessary for stability.

**References**
- **PROJECT RULE:** this is a product/latency rule for interactive voice behavior in this repository, not a strict upstream library contract.

---

## 7. Prefer graceful polling for transient disconnects

In loops such as `next_event()`:

- temporary absence of session MUST be treated as a transient condition
- you SHOULD prefer short non-blocking delay and retry-style behavior where appropriate
- you MUST NOT collapse outer orchestration loops on every temporary gap

**References**
- **PROJECT RULE:** this is a control-plane resilience policy for this repo’s orchestration loops.

---

## 8. Scope concurrency changes narrowly

You MUST NOT do workspace-wide mutex substitutions.

Preferred order:
1. fix lock lifetime first
2. optimize lock type second

You MUST keep Tokio locks in async orchestration code, use `parking_lot::Mutex` ONLY for proven sync hot paths, and use `std::sync::Mutex` for small sync-only state.

**References**
- This rule follows the division of responsibility documented by Tokio for async mutexes and by `parking_lot` / `std` for blocking mutexes. [^tokio-mutex] [^parking-lot-mutex] [^std-mutex]
- **PROJECT RULE:** “no workspace-wide substitutions” is a repo policy intended to prevent broad, low-signal lock churn.

---

[^tokio-mutex]: Tokio `Mutex` docs: <https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html>
[^tokio-rwlock]: Tokio `RwLock` docs: <https://docs.rs/tokio/latest/tokio/sync/struct.RwLock.html>
[^std-mutex]: Rust standard library `Mutex` docs: <https://doc.rust-lang.org/std/sync/struct.Mutex.html>
[^parking-lot-mutex]: `parking_lot::Mutex` docs: <https://docs.rs/parking_lot/latest/parking_lot/type.Mutex.html>
[^tokio-mpsc]: Tokio `mpsc` docs: <https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html>
[^futures-sink]: `futures::Sink` trait docs: <https://docs.rs/futures/latest/futures/sink/trait.Sink.html>
[^bytes]: `bytes::Bytes` docs: <https://docs.rs/bytes/latest/bytes/struct.Bytes.html>
[^bytesmut]: `bytes::BytesMut` docs: <https://docs.rs/bytes/latest/bytes/struct.BytesMut.html>
[^tokio-block-in-place]: Tokio `block_in_place` docs: <https://docs.rs/tokio/latest/tokio/task/fn.block_in_place.html>
[^tokio-spawn-blocking]: Tokio `spawn_blocking` docs: <https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html>
