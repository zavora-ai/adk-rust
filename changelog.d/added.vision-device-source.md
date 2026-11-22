- **Local cameras as frame sources** (`adk-vision`, `adk-cli`): `DeviceSource`, behind the
  new `device` feature, captures a camera on this machine through a user-installed `ffmpeg`
  — AVFoundation on macOS, v4l2 on Linux, DirectShow on Windows — at a requested capture
  rate and size, with the stream's sampling, drop-oldest backpressure, and the `ffmpeg` child
  killed when the stream is dropped. `DeviceSource::list` names the video devices without
  camera permission, and `DeviceSource::test_pattern` stands in for a camera.
  `adk-rust vision sources list` and `vision sources test <source>` list the cameras and
  probe a camera, an RTSP URL (credentials redacted) or a video file, storing nothing.
