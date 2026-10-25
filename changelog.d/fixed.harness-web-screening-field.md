- **Web results say what screening did** (`adk-harness`): `web_fetch`, `web_search` and the
  browser pack's results carry `screening` — `clean`, `labelled`, `redacted`,
  `labelled, redacted`, or `not_screened` for a PDF stub — in place of `screened: bool`,
  whose `false` read as "not screened" when it meant nothing was flagged; the
  `harness.web.fetched` and `harness.browser.acted` records carry the same field
  (spec 3.1 §7.33 fix 4).
