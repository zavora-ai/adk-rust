- **Voice surface for the harness** (`adk-harness` feature `voice`, spec 3.1 §7.17, H5):
  `VoiceSurface` runs a `HarnessAgent` marked `Pack::Voice` as the agent of an
  `IntegratedRealtimeRunner`, with the harness tools as the runner's tools and
  `HarnessAgent::run_config()` as its governance, so a spoken request runs under the
  session's mode and standing rules. A held call is surfaced as a realtime event and
  recorded in the session and the approval store; a spoken "yes" or "no"
  (`parse_spoken_decision`) or the approvals inbox decides it, and `harness.voice.held`,
  `harness.voice.decided` (with the transcript's digest) and `harness.voice.resumed` join
  the tool's own records in one audit trail. `store_to_memory` and memory injection follow
  the harness memory policy (off). `adk-rust --voice "…"` is the console surface (adk-cli
  feature `voice`; `--voice-input <wav>` answers the first held call from a recording,
  `voice-desktop` adds the microphone and speaker).
