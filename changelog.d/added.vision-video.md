- **Video and tracking** (`adk-vision`, spec 3.7 phase B): `ByteTracker` (feature `track`)
  runs ByteTrack through `trackforge` 0.5 for anonymous tracks that keep their identifiers
  through detector jitter and occlusions; `VisionRuntime::analyze_video` holds a call's
  frames against the budget as `UsageUnit::Frames` before anything leaves, and
  `EgressGate::release_clip` releases an MP4 or MOV `SourceClip` only after the destination
  and rule checks, refusing every clip while redaction is required. `GeminiVideoAnalyzer`
  (feature `gemini-video`) answers questions with Gemini native video, uploading clips above
  20 MB once through the File API and sending a file part at an explicit frame rate and media
  resolution; `FramesAnalyzer` (feature `cloud`) asks any vision-language model, Qwen3-VL on
  the device included, about sampled frames and names events by frame; `SamVideoTracker`
  (feature `sam`) tracks objects through a clip with `sam-3.1` video, with stable identifiers.
  Each analysis reports its frames, tokens and cost.
- **Gemini native video** (`adk-model`, `adk-gemini`): a user `Part::FileData` with an
  `https`, `http` or `gs` URI goes to Gemini as native `fileData` instead of a text
  reference; `video_part_annotations` sets a video part's `videoMetadata` (frame rate and
  offsets); `GeminiRequestOptions` sets `generationConfig.mediaResolution`;
  `GeminiModel::upload_file`, `wait_for_file`, `get_file` and `delete_file` use the File API.
  `adk-gemini` adds `VideoMetadata` and `MediaResolution`.
- **`sam-3.1` video price** (`adk-model`): $0.20 per 1,000 frames with a 50-frame minimum per
  request, metered as `UsageUnit::Frames`.
