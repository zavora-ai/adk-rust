- **Web home decision cards show bidi and zero-width characters as escapes** (`adk-surface`):
  the exact call, the heading, the timeline and the Edit field write each control, bidi
  override or isolate, zero-width character and byte-order mark from a tool's arguments as
  its `\u{..}` escape, as the terminal does, so a call can no longer read differently from what
  runs. The Edit field writes them as JSON `\uXXXX` escapes, which parse back unchanged.
