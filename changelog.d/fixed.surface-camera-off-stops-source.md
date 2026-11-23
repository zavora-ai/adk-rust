- **Switching a stream camera off stops its capture** (`adk-surface`, `vision-camera`): a
  `PipelineCameras` stream camera closes its source before **Off** answers, so a
  `DeviceSource`'s ffmpeg exits and the camera light goes out, and an RTSP connection closes;
  **Camera on** opens it again. Both are recorded as `vision.source.stopped` and
  `vision.source.started` with the reason. `PipelineCameras::is_capturing` reports whether a
  camera's source is open.
