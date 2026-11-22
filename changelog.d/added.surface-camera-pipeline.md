- **The live camera surface on the frame pipeline** (`adk-surface`, feature `vision-camera`):
  `camera::PipelineCameras` runs each camera through `adk-vision`'s `StreamPipeline` and is the
  `CameraSource` the hub reads, so the picture, its overlays, the tracks, the frame governor's
  rate, quota and steps, and the watch conditions and zones are what the pipeline measured. A
  zone edited on the surface becomes the watch conditions' zone from the next frame; a camera
  switched off has its frames dropped before the pipeline looks at them; a lost source is retried
  with a growing wait or at once from **Try again now**, its error shown with credentials hidden.
  The voice screen's camera reaches its pipeline through `Cameras::ingest` and `adk-vision`'s
  `SurfaceCameraSource`, and its switch follows the voice screen's. `live_camera_home` serves the
  conveyor clip over RTSP through two cameras, and `web/e2e/camera.spec.ts` drives it end to end.
- **Stop all pauses every camera** (`adk-surface`, spec 3.7 V-I17): the hub's gate drops every
  frame while Stop all is in force (`Cameras::freeze_all`, `DropReason::Frozen`) or while the kill
  switch it follows (`Cameras::with_governance`) is frozen, and the camera shows "Paused by Stop
  all" until Resume all.
