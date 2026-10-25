- **Settings changed in the home, chat cards and "While you were away" behave as the person
  sees them** (`adk-kernel`, `adk-cli`): a mode, quiet time or spending limit changed in the
  home gives way to a different start value (`adk-rust up --mode`, `partitions set-mode` and
  a restart); concurrent changes of one area all hold (`ControlStore::update_area_settings`,
  compare-and-swap on every backend); admission keeps the last settings it read through a
  control-store outage; a tightened mode holds the overnight tidy-up and the kernel's
  questions back; chat cards follow the quiet time set in the home, are sent again when a
  send fails, wait on the wall clock rather than a timer a sleeping laptop stops, and a turn
  another replica ran still gets its reply; "While you were away" judges quiet time by the
  window in force when work started, names agents as they call themselves, serves only the
  rows of its lines and accepts any `after`; a device's answer is recorded with its device
  and answer, and record lines carry a kernel record's action (`kernelAction`); a refused
  pairing request is no longer recorded as issued; `pair accept` escapes the account's name.
  Spec 3.3 §5.1, §7.3, §8, §10 K1–K4, §11, S-I9.
