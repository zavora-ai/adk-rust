- **Artifact errors are typed on every backend** (`adk-artifact`): `InMemoryArtifactService`
  and `FileArtifactService` reported a missing artifact and an invalid name such as
  `../escape.txt` as `artifact.legacy` with `ErrorCategory::Internal`, so
  `AdkError::is_not_found()` was false and a caller could not tell bad input from a storage
  fault. A missing artifact or version now fails with `artifact.not_found` (`NotFound`), an
  invalid file name with `artifact.invalid_name` (`InvalidInput`), and an invalid app name,
  user ID, or session ID with `artifact.invalid_identifier` (`InvalidInput`).
  `GcsArtifactService` uses the same codes for the same failures in place of
  `artifact.gcs.not_found` and `artifact.gcs.invalid_input`, which remain for failed GCS
  requests. Filesystem failures in `FileArtifactService` use `artifact.file.io`. Code that
  matched `artifact.legacy` or the GCS codes for these cases matches the new codes.
