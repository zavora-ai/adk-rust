- **Video analyst** (`adk-vision`, feature `analyst`; spec 3.7 §7.13, §7.15, scenario S17):
  `VideoAnalyst` cuts a video from any frame source into fixed windows or scene cuts
  (`Chunking`), captions each chunk through the runtime's video analyzer — on the device, or
  remotely with every frame through the egress gate — and indexes it as an `adk-rag` chunk in
  any vector store with its time range, keyframe and clip, its caption in `adk-memory` with
  artifact references, and the clip in graph memory. `ask` answers a question from the
  retrieved chunks' captions, citing chunks with their time ranges; `IndexedClip` reports the
  captioning's cost per hour of video; `erase_source` removes a clip's chunks, captions, graph
  entity and keyframes with a `vision.analyst.erased` record. Keyframes pass the privacy hook
  before they are stored and can be embedded in a `VisualMemory`. `WordHashEmbedding` is a
  text embedding that needs no model, and `analyst::fixtures::WarehouseClip` draws scenario
  S17's hour of warehouse video with ten labelled events.
- **Captions over sampled frames** (`adk-vision`, feature `cloud`): `FramesAnalyzer` answers
  `VideoRequest::Caption`, one call per chunk over at most `frames_per_call` of its frames,
  naming what each chunk shows as the event's `labels` (`VideoEvent::labels`, new);
  `with_caption_instructions` replaces `DEFAULT_CAPTION_INSTRUCTIONS`. The video conformance
  suite gains a caption case (`VideoFixture::with_caption`).
