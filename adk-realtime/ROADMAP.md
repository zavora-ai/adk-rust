# adk-realtime Roadmap

## Realtime Audio Transport (v0.3)

Three new transport capabilities behind feature flags, all additive to existing APIs.

### Completed

- [x] **Error types** — `OpusCodecError`, `WebRTCError`, `LiveKitError` variants with convenience constructors
- [x] **Feature flags** — `vertex-live`, `livekit`, `openai-webrtc` in Cargo.toml with `full` composite
- [x] **Vertex AI Live backend** — `GeminiLiveBackend::Vertex` with OAuth2/ADC auth, `build_vertex_live_url()`, WebSocket connection via `google-cloud-auth`
- [x] **LiveKit WebRTC bridge** — `LiveKitEventHandler<H>`, `bridge_input()`, `bridge_gemini_input()` for provider-agnostic LiveKit room integration
- [x] **OpenAI WebRTC transport** — `OpenAIWebRTCSession` with `str0m` Sans-IO WebRTC, `OpusCodec` wrapper, SDP signaling, data channel events, `OpenAITransport` enum
- [x] **Backward compatibility** — Existing `openai`, `gemini`, and default features unchanged
- [x] **Examples** — `vertex_live_voice`, `livekit_bridge`, `openai_webrtc`
- [x] **Integration tests** — `#[ignore]` tests for all three transports with timeout guards
- [x] **Property tests** — 5 properties validated with `proptest` (100 iterations each):
  1. Vertex URL construction
  2. LiveKit event handler delegation
  3. Opus codec lossy round-trip
  4. SDP offer structure
  5. Error message context preservation
- [x] **Facade crate** — `adk-rust` forwards `vertex-live`, `livekit`, `openai-webrtc` to `adk-realtime`, prelude re-exports for realtime types

### Requirements Reference

Full requirements, design, and task breakdown are in:
- `.kiro/specs/realtime-audio-transport/requirements.md` — 18 requirements
- `.kiro/specs/realtime-audio-transport/design.md` — Architecture, components, 5 correctness properties
- `.kiro/specs/realtime-audio-transport/tasks.md` — 13 top-level tasks with sub-tasks

### Known Issues

- `audiopus` requires `cmake` at build time. With cmake >= 4.0, set `CMAKE_POLICY_VERSION_MINIMUM=3.5` to work around the bundled Opus CMakeLists.txt compatibility.
- Integration tests require real credentials/services and are marked `#[ignore]`.

### Future Work

- Token refresh for long-lived Vertex AI Live sessions
- Automatic reconnection on transport failures
- Audio resampling utilities (beyond the Gemini 16 kHz bridge)
- WebRTC ICE candidate management for NAT traversal scenarios
- Metrics/telemetry integration for transport-level observability
