- **The tool-progress truncation marker reaches a client that fell behind** (`adk-agent`): when
  a tool's progress queue stayed full past the 100 ms send wait, the agent stopped forwarding
  that call's progress and tried to queue `[adk: tool progress truncated]` without waiting, so
  the marker was dropped in the full-queue case it reports. A marker that finds the queue full,
  this one or `[adk: forwarded events dropped]`, is now held and delivered after the queued
  progress events, before the tool's result.
