- **Stop all closes local camera sources** (`adk-surface`, spec 3.7 V-I17): under
  `vision-camera`, Stop all now closes each stream camera's source as switching it off does — a
  local camera's ffmpeg exits, an RTSP connection closes — before it answers, recorded as
  `vision.source.stopped` with reason `stopAll`; Resume all opens again each source whose
  camera is on (`vision.source.started`, reason `resumeAll`). Before, Stop all only dropped
  frames and a local camera kept capturing.
