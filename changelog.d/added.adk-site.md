- **`adk-site`: the site pipeline of an AWP 2.0 website** (spec 3.5 §7.7, §7.8, §7.8.1,
  §7.8.2, Experimental): a new crate. Content items are Markdown files with front matter in
  `content/<kind>/<slug>/<language>.md`, behind `ContentStore` (filesystem and in memory) and
  `ContentHistory` (files and in memory), each with a conformance suite. `BuiltinGenerator`
  renders pages per language with a Markdown twin and a structured `MachineView` twin of every
  page, `llms.txt`, `robots.txt` with `Content-Usage` from the charter's public terms,
  `security.txt`, sitemaps with `lastmod`, Atom feeds, schema.org JSON-LD per content kind and
  redirects in the host's format; identical inputs give byte-identical files, and incremental
  builds rebuild an item and its dependents only. `LaunchGate` runs the §7.8.1 checks in fast
  and full tiers, with claims checked through a caller's `ClaimCheck`. `ReleaseBuilder` hashes
  a build into a snapshot or delta `ReleaseManifest` and refuses any server-executable file or
  server-side include directive, sniffed by content (A-I1). `SiteLog` keeps `{seq, manifest}`
  entries as C2SP tiles with checkpoints signed by a `ReleaseSigner` (AWP 2.0 §14.5), with
  inclusion and consistency proofs computed from published tiles alone. `DirectoryHost`
  publishes atomically and rolls back to exact digests under the `SiteHost` conformance
  suite. `PublishingPipeline` runs draft, check, preview, approve, release, publish, announce
  and measure, with corrections, withdrawals, schedules, per-item revert and whole-site
  rollback; `MediaPipeline` refuses executables and polyglots and strips location metadata.
