- **The desktop tray follows the web home's palette** (`adk-surface` desktop): its windows' own
  background and its bundled pages still used the palette the web home left in its redesign, so
  a window showed the old colour before its page painted. Both now take the web home's
  `--bg`, `--card`, `--raised`, `--border`, text, label, brand and accent tokens, light and dark.
