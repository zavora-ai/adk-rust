- **The spec 3.5 scenarios as a runnable example** (`examples/website`): product-level runs
  of `adk-website` for S1–S19 against local fixtures, scripted by default and with `--live` on
  `claude-sonnet-5-5` or `gemini-3.7-flash`; the WordPress compose fixture with its `infected`
  overlay, the business-presence pages and recorded inventories; the S19 soak and its report.
  The pull-request tier runs the scripted scenarios (`website-scenarios`); the nightly tier runs
  the WordPress fixture (`website-wordpress`) and the soak (`website-soak`).
