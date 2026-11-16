- **`adk-realtime`'s `livekit` builds again** (`adk-realtime`): `webrtc-sys-build` 0.3.21
  downloads a libwebrtc whose headers lack `webrtc::ArrayView`, which the pinned
  `webrtc-sys` 0.3.40 needs, so a fresh resolve no longer compiled. `livekit` now pins
  `webrtc-sys-build = "=0.3.18"` beside the `webrtc-sys` pin, so consumers' resolves pick the
  matching release too.
