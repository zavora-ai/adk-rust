- **Artifact versions are immutable, and the session id `user` is reserved**
  (`adk-artifact`): a save with `version: Some(n)` for an existing `n` fails with
  `ErrorCategory::Conflict` (`artifact.version_exists`) on every backend instead of
  replacing the stored version. Every operation on every backend rejects the session id
  `user` (`USER_SCOPE_SESSION_ID`) with `ErrorCategory::InvalidInput`
  (`artifact.reserved_session_id`): it names the segment user-scoped artifacts are stored
  under, so a session with that id leaked its artifacts into every other session's
  listing. Artifacts saved earlier under the session id `user` are no longer reachable.
- **`FileArtifactService` writes a new on-disk layout** (`adk-artifact`): artifact names
  are encoded injectively (`%`, `:`, the other characters Windows forbids in file names,
  and the second `_` of every `__` become `%XX` escapes) and user-scoped artifacts live
  under `{app}/{user}/user/` instead of `_user_scoped_`. Artifacts in the earlier layout
  are still loaded, listed, versioned, and deleted, and new versions of them continue
  their numbering in the new layout. Tools that read the directory tree directly need
  the new paths.
