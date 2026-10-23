- **Fact consolidation** (`adk-harness`, spec 3.2 §7.5, §10 HK2): `Harness::consolidate_memory`
  runs the consolidation tick's work on long-term facts. It distils the sessions the
  per-session distiller never reached with verified-quote promotion off, so every fact it
  writes is `Assumed`; pass 1 supersedes the older of two `Assumed` near-duplicates (cosine at
  least `dedup_score`, 0.92) and expires unused, low-confidence `Assumed` facts older than
  `assumed_ttl` (90 days), without a model call; pass 2 merges clusters of similar `Assumed`
  facts on the distiller model, the merged fact listing its sources in
  `Provenance::source_ids`. `Known` facts are never superseded, merged or expired, an
  `Assumed` fact naming a `Known` one is stored beside it, and the passes apply nothing past
  `max(1, ⌊max_loss_fraction × live facts⌋)` lost live facts in a scope (`max_loss_fraction`
  0.25): one per pass-1 supersession or expiry, and per merge the facts it supersedes less
  the one it writes, so a small scope can still collapse one duplicate. Each tick keeps a `ConsolidationRecord`
  in the fact store and writes one `harness.memory.consolidated` audit record;
  `Harness::undo_consolidation` and `undo_consolidation` revert a finished tick
  (`ConsolidationRecord::finished_at`); one still running is refused.
  `Harness::distill_sessions` is the distillation entry on its own; on a harness that
  requires authenticated identity, it distils a session that ran in an earlier
  process only when the session's latest run in the harness's run store was authenticated. `FactStore` gains
  `distilled_sessions`, `mark_distilled`, `put_consolidation`, `consolidations` and
  `restore_assumed`, implemented by the in-memory, file and SQLite stores, and
  `fact_store_conformance_tests!` three cases for them.
