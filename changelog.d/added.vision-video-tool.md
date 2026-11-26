- **`vision_video` tool** (`adk-vision`): the governed toolset's tenth tool hands the runtime's
  video route 1 to 64 frames the session holds, oldest first with their times, and asks a
  question, captions them in chunks or tracks objects by concept across them. Event times
  come from the frames' times; tracked regions name their frame, time, digest and track. For
  a remote analyzer each frame passes the egress gate, and a rule that asks holds the call
  once with every frame as evidence. The harness vision pack screens its answers and captions
  as untrusted text. See `docs/official_docs/vision/governed-tools.md#vision_video`.
