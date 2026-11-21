- **`adk_gemini::Part::InlineData` and `Part::FileData` carry `video_metadata`**
  (`adk-gemini`): construct them with `video_metadata: None` and match them with `..`.
  `adk_vision::VideoAnalyzer::analyze` returns `VideoAnalysis<RawRegion>`, and
  `VideoInput` gains the clip a video backend reads.
