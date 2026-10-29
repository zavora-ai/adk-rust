- **`FileAuditRecorder` cuts an unfinished line on Windows** (`adk-core`, spec 3.5 §9.3): an
  append writes at the end of the file under its exclusive lock instead of through an
  append-only handle, which Windows does not let truncate, so a line a crash left unfinished is
  cut before the next record there as on Unix, where every append after it used to fail.
