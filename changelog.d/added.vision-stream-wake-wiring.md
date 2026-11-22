- **A stream wakes an agent** (`adk-vision`, `adk-harness`, `adk-kernel`, `adk-rust`): `StreamWatch` is the stream pipeline's watch stage — watch conditions held
  against every observation, the frame of each event kept redacted as its wake payload, the
  event naming the kept artifact, a `vision.watch.fired` record, and delivery to a
  `WatchSink` such as the kernel's `VisionWakeSource`. `FrameSources` holds the operator's
  named frame sources and streams each only while a watch is registered on it; the
  `vision_watch` tool (`VisionToolsetBuilder::frame_sources`, `VisionPack::frame_sources`)
  lists them and starts or stops a watch, writing `vision.watch.registered` and
  `vision.watch.stopped`. Stream detector calls are metered in `UsageUnit::Frames`
  (`CallOptions::as_frames`). `Tracker::live_tracks` and `Observation::tracks` keep a track's
  watch state through a frame the detector misses, so it does not fire again.
  `WatchEvaluator::add` and `remove` change conditions while a stream runs. With features
  `computer-use` and `screen`, `adk_vision::stream::RuntimeObserver` and `screen_source` make
  any `ComputerUseRuntime` a screen frame source. The umbrella `vision` feature now forwards
  the frame sources (`rtsp`, `ffmpeg`, `screen`, `browser`, `surface-camera`), `stream`,
  `track` and `gemini-video`, and the kernel's `vision` feature. See
  `docs/official_docs/vision/streams.md` and `docs/official_docs/vision/wake.md`.
