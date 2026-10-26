- **`RsyncHost` keeps a read-only copy per release** (`adk-site`, `host-rsync`): unchanged
  files are copied on the server (`rsync --copy-dest`) instead of hard-linked into the next
  release, and every served file is read-only, so a write through one release's file no longer
  changes the releases that shared it and a rollback finds its files as they were published.
  Only the log's tiles and manifests stay shared, read-only (spec 3.5 §7.8, A-I11).
