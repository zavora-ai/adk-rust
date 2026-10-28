- **Five-field cron expressions count weekdays as Unix cron does** (`adk-core`): in
  `ScheduleSpec::Cron`, the day-of-week field of a five-field expression reads 0 and 7
  as Sunday and 1 to 6 as Monday to Saturday, so `0 9 * * 1-5` fires Monday to Friday
  (spec 3.3 §12 phase 4). It fired Sunday to Thursday before, under the `cron` crate's
  numbering, which refused 0. Six- and seven-field expressions keep the crate's
  numbering (1 is Sunday); day names mean the same days in both. The change reaches
  every cron schedule built on `adk-core`: `adk-rust wake --cron`, kernel routines,
  `adk-server` cron jobs and the scheduler.
