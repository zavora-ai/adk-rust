- **Concurrent artifact saves each keep their version** (`adk-artifact`): saves of one
  artifact that ran at the same time returned the same version and all but one were
  lost. `InMemoryArtifactService` chooses and inserts the version under one write lock,
  `FileArtifactService` claims the version file without replacing an existing one, and
  `GcsArtifactService` uploads with `ifGenerationMatch=0`; a save that loses the race
  moves on to the next version.
- **File artifact writes are crash-safe** (`adk-artifact`): `FileArtifactService` writes
  each version to a temporary file, flushes it to disk, and links it into place, so a
  crash mid-save no longer leaves a truncated latest version that fails to load. On
  filesystems without hard links (FAT, exFAT) the version file is created exclusively and
  written in place instead.
