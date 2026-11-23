- **Vision scenarios S9–S13** (`examples/vision`, `vision-scenarios`): spec 3.7's phase B
  scenarios on the conveyor clip served over the in-process RTSP server — the Inspector (one
  track and one wake per seeded defect, only woken frames stored, SAM 2.1 Tiny masks measured
  against the fiducial within 5%, RF-DETR loaded from a user-written manifest), a harness
  process woken by `VisionWakeSource` whose line stop is held with the frame and runs after one
  approval, the frame governor across midnight with `sam-3.1` video escalations billed 50
  frames each, the live camera surface driven without a browser, and native video two ways
  (Gemini through the File API, Qwen3-VL over sampled frames after a visibility pass). The
  `vision-scenarios` job runs them; `docs/official_docs/vision/scenarios.md` lists what each
  asserts and what stands in for what.
