- **Constantly updated sites in `adk-site`** (spec 3.5 §7.8.3, A-I26, A-I27, Experimental):
  per-kind field lanes in `site.toml` (`[lanes.<kind>] volatile`, `stream`, `claims`) with claim
  fields refused outside the release lane by `SiteConfig::validate` and every build;
  `LiveStore` for the live lane, whose last value and time released pages show with script off;
  `Batcher` with windows per kind (30 s, articles 10 s, products 60 s), `flush_now` triggers,
  `max_items` and `max_bytes`, the latest version of an item winning and earlier ones kept in the
  content history only; `ContentSource` (`InMemorySource`, `CsvFileSource`) and model-free
  `FeedMapping`s with declared types, units and bounds; `feed::bounds::check`, which holds a
  delivery that breaks its `FeedBounds` whole with a P2 alarm and a change summary;
  `PublishingPipeline::publish_batch`, which holds a fast-tier failure alone, refuses the whole
  release on server code and publishes one delta or snapshot release per batch with one
  `catalog.changed` paginated beyond 1,000 ids (`Announcement::catalog_pages`,
  `PublishingPipelineBuilder::changes_url`); `PublishingPipeline::full_check`; and
  `UpdateEngine`, which holds price-rule exceptions and new items alone, runs the full checks
  every six hours or 500 releases, reverts one item (`revert`), lists and replays changes after
  a rollback (`rollback`, `replay`), and reports batches, feeds and cost per thousand updates.
  `batch::check_host` refuses a host without atomic publication for windows under a minute.
  `DirectoryHost` stores every file version once under its digest and switches a pointer last
  (`Atomicity::PrefixPointer`); `materialise(false)` drops the directory per release and
  `retain_releases(n)` bounds storage. The host conformance suite gains
  `atomic_delta_under_concurrent_reads`.
