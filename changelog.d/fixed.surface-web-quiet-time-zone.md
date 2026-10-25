- **The web home reads and saves quiet time on the device's clock** (`adk-surface`): a window
  saved from **Memory & rules** carries the browser's offset, so the kernel no longer enforces
  it in the server's time zone, and a window without one is shown, and checked for the status
  line, the morning briefing and Today, at the server's offset, which `StatusLine` now carries
  as `hostUtcOffsetMinutes`. Before, a home opened in another zone than the server said quiet
  time held at hours the kernel did not keep it.
