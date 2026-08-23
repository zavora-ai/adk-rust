# Ambient scheduling

`ambient_monitor` is both selectable in the interactive interface and driven in
the background by an `AmbientAgent`. A cron trigger fires every 30 seconds and
uses `RunnerTriggerConfig` to invoke the exact same root through `Runner`.
Override `ADVANCED_AMBIENT_CRON` with another six-field schedule when needed.

The trigger uses `local-user` and a shared `ambient-monitor` session so the UI
can discover it without changing identity. The Sessions, Timeline, and
Telemetry tabs refresh automatically every four seconds.

Expected behavior:

1. Select `ambient_monitor`.
2. Wait for the `ambient-monitor` session to appear automatically.
3. Inspect the generated operational pulse in Timeline.
4. Open Telemetry to inspect the corresponding Runner/model spans.

The shared session is deliberate for the demo. Production schedules should
prefer `TriggerSessionPolicy::PerTrigger` unless accumulated history is needed.
