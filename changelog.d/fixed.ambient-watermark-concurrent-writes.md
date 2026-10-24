- **Concurrent writes of a `FileTickWatermark` no longer fail on Windows** (`adk-agent`):
  two writes replacing the same watermark file at once failed with `Access is denied` on
  Windows, where a replace fails while another replace of the file is in flight. Writes
  through one watermark and its clones now take turns, and on Windows a write retries a
  replace another process holds for up to `FileTickWatermark::PUBLISH_RETRY` (1 s) before
  it fails with an error that names the conflict. The last write wins, as before.
