- **`adk-realtime`'s `livekit` builds again** (`adk-realtime`): a fresh resolve picked
  `webrtc-sys-build` 0.3.21, whose libwebrtc headers lack the `webrtc::ArrayView` that the
  pinned `webrtc-sys` 0.3.40 needs, plus `livekit-protocol` 0.7.13 and `livekit-common` 0.1.7,
  which livekit 0.8.1 no longer compiles against. `livekit` now pins `webrtc-sys-build`
  `=0.3.18`, `livekit-protocol` `=0.7.12` and `livekit-common` `=0.1.1` beside the existing
  livekit pins, so consumers' resolves pick the matching releases too.
