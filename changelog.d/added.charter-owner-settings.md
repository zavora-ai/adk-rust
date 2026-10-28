- **The owner's settings as charter members** (`awp-types`, `adk-website`, spec 3.5 §7.6):
  `Charter` gains the optional `handoffThreshold` (a lead score from 0 to 100),
  `publishDelayHours` and `amountThresholds` (capability → minor units), with
  `CharterViolation::HandoffThresholdOutOfRange`; the committed JSON Schema follows. A
  charter without them reads and validates as before. `adk-website` shows them on the charter
  card "When the site asks you" and reads them from the charter in force —
  `Leads::with_charters`, `PublishDelay::with_charters`, and the edge's amount thresholds —
  falling back to 70, `SiteOptions::publish_delay` (24 hours) and `approvals.json`.
