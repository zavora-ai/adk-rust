- **This browser's camera on the live camera page** (`adk-surface`): `POST
  /surface/cameras/{camera}/frames` takes a frame a screen captured for a camera that takes
  them (`LiveCamera::surface`, `CameraActions::send_frames`), through the hub's gate, with no
  voice screen or realtime model. The camera page's **Use this browser's camera** switches the
  camera on and sends five JPEGs a second; switched off anywhere, it stops capturing. Owner
  only, the decide scope on a paired phone, JPEG, PNG or WebP up to 2 MiB, at most
  `MAX_SCREEN_FPS` (10) a second.
