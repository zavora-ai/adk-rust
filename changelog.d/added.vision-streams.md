- **Frame streams for `adk-vision`** (`adk-vision`, features `stream`, `rtsp`, `ffmpeg`,
  `screen`, `browser`, `surface-camera`, `stream-fixtures`; spec 3.7 §7.9, §7.13): the
  `FrameSource` trait and five sources — RTSP cameras through `retina` (MJPEG decoded in
  process, H.264 and H.265 through a user-installed `ffmpeg`), MP4 and MOV files through
  `ffmpeg` (never vendored or downloaded), screenshots from a `ComputerActuator` or
  computer-use-mcp observations, `browser_screenshot`, and the web home's camera through a
  `CameraFeed`. `StreamPipeline` runs each stream through sampling, a `FrameGovernor` that
  lowers the local rate, then stops escalation, then pauses the source under its frame
  quotas and resumes it next period, a required privacy hook before any other stage, the
  detector on the governed runtime, a tracker and an `ObservationSink`. `FrameKeeper` stores
  only redacted frames, for 30 days by default, and purges them on schedule. Every source,
  governor and evidence change writes a `vision.*` audit record, and the kill switch pauses a
  stream at its next frame. `stream-fixtures` adds a generated conveyor clip with seeded
  defects and an in-process RTSP test server.
