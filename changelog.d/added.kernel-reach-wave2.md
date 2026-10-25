- **Quiet hours in a named time zone, and the agenda newest first** (`adk-kernel`):
  `QuietHours::time_zone` (`"timeZone": "Europe/London"` on the wire) reads a window in an
  IANA time zone, so a window saved from the home keeps its local times when the clocks
  change, where a fixed `utcOffsetMinutes` moves it by an hour; `GET /kernel/agenda` takes
  `newestFirst=true` and `after=<item>` (`AgendaQuery::newest_first`, `AgendaQuery::after`),
  so a surface reads the newest items however many older ones the store keeps. Spec 3.3
  §5.1, §10 K3.
