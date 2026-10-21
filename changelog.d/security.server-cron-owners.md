- **Cron jobs belong to their caller** (`adk-server`): with a `RequestContextExtractor`, a
  job created through `/api/cron` belongs to the authenticated caller — another owner's job
  answers `404` to `GET`, `PATCH`, and `DELETE` and is left out of the list, two owners may
  each have a job of the same name (the schedule id is `{len}:{owner}:{name}`), and its runs
  act for the owner instead of the user `cron`. Before, any authenticated caller could read,
  pause, or delete any job. A job stored before this change under its bare name is no longer
  reachable through the API on a server with an extractor; delete it through the
  `ScheduleStore` and create it again.
