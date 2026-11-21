- **The live camera surface** (`adk-surface`, spec 3.7 §8.2): watching a camera an agent watches
  and acting on it, on the web home (`/ui/cameras/<camera>`, `/ui/cameras`) and the phone, built to
  the signed-off boards. Live frames with overlays, tracks, watch conditions and zones, the frame
  governor's rate, escalations and quota, and the source switch; a person switches a camera off,
  edits a zone, stops the stream and raises a card from a frozen frame. `camera::Cameras` drops
  every frame of a camera switched off on the server and holds the rest in memory only;
  `CameraSource::keep_frame` keeps a card's one frame, everyone in it covered first, as the
  evidence the card and a held call carry. Routes under `/surface/cameras`, `ScriptedCameras` for
  fixtures, `camera::bridge::feed_realtime` (`voice`) for a realtime session under the voice
  screen's `FramePolicy`, and the `scripted_camera_home` example.
