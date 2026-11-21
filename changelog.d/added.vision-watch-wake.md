- **Watch conditions and visual wakes** (`adk-vision`, `adk-kernel`): `WatchCondition`
  (labels, minimum score, minimum consecutive frames, optional zone polygon, cooldown) and
  `WatchEvaluator` hold conditions against tracked observations on the device and return one
  `WatchEvent` per tracked object, keyed by source, session, track, condition and kind.
  `VisionWakeSource` in `adk-kernel` turns each event into a wake keyed
  `queue:<source>:<session>:<track>:<condition>:<event>`, which `Wake::Queue` matches with no
  new `Wake` variant: a sleeping process wakes once per object, a `Trigger` starts a
  `Triggered` process when none sleeps, and the payload names frame artifact, digest, track and
  the top regions within `WAKE_PAYLOAD_LIMIT`. Feature `vision` of `adk-kernel` makes the
  source an `adk-vision` `WatchSink`; `vision.watch.registered` and `vision.watch.fired` audit
  drafts name the condition by digest. See `docs/official_docs/vision/wake.md`.
