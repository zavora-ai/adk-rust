- **Voice decisions on parked kernel processes** (`adk-harness` features `voice` and
  `kernel`; spec 3.2 §7.11): `VoiceSurface::with_parked_processes` answers processes the
  kernel parked on a held call through the kernel's approval service, whose run store wakes
  the run. `parked_processes` lists them, `offer_parked` puts one up for the next spoken yes
  or no, and `decide_process` decides one from code; the session's own held calls are
  answered first. A process a freeze parked is refused with `harness.voice.frozen` and a
  `harness.voice.refused` record: only `unfreeze` wakes it (K-I21).
