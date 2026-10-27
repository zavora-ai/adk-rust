- **`RsyncHost` reads survive a release switch on macOS** (`adk-site` `host-rsync`, spec 3.5 §7.8
  "Hosts"): a read through `current` while a publish renames the link can fail once with EINVAL
  on APFS although the rename is atomic; `fetch` reads again, up to twice, so a reader during a
  publish sees one release or the other. The host conformance suite ran 30 times in parallel
  with no failure, where it failed once in 12 before.
