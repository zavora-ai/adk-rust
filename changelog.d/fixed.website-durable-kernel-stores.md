- **A served site's kernel keeps its process table across a restart** (`adk-website`): the
  kernel `deploy::serve` and `deploy::serve_agency` start kept its process table in memory
  while its runs were durable, so after a restart a process asleep on a wake set, quiet hours
  or a spent budget stayed `Waiting` and the day's spend totals reset. `SiteStores` now opens
  `processes` (`SqliteProcessStore`) and `schedules` (`SqliteScheduleStore`) in
  `data/kernel.db`, and the served kernel gets them with the spend ledger and an audit trail:
  the site's `data/audit.jsonl`, or the agency's. At start, `serve` logs each job whose process
  it does not schedule, with the reason.
