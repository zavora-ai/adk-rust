- **`rtsp` builds again from a fresh resolution** (`adk-vision`): pretty-hex 0.4.3 added a field to
  `HexConfig`, which `retina` builds with a struct literal, so a project resolving its
  dependencies afresh failed to compile `retina`. The `rtsp` feature caps pretty-hex below 0.4.3.
