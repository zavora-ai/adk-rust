- **S13 and S17 on the device, recorded live** (`adk-vision`, `examples/vision`): S13's
  local path replays Qwen3-VL-4B's own run on an Apple M4 instead of a scripted transcript
  — ten calls over 240 frames answering 12:50 for the entry at 12:45, 706 s on the CPU and
  61 s with `mistralrs-metal`, the same answers — and checks that its spend ledger stays at
  zero. `analyst_live_tests` captions and answers S17's hour with Qwen3-VL-4B on the device
  (`ADK_VISION_ANALYST_CHUNKS` bounds it), and `analyst_tests` replays that run: every
  event's chunk captioned with its event, 8 of 10 answers citing it, nothing sent or spent.
  `video_local_live_tests` records its run with `ADK_VISION_RECORD`, and the live tests'
  recorders forward the model's cost bound.
