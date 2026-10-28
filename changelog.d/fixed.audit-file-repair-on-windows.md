- **The file audit recorder repairs a torn line on Windows** (`adk-core`):
  `FileAuditRecorder` cuts a line a crash left unfinished and keeps recording. On
  Windows the cut failed with "Access is denied", and every later record failed with it.
