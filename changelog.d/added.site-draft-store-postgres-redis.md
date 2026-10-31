- **Website drafts on PostgreSQL and Redis** (`adk-harness`): `PostgresSiteDraftStore`
  (feature `postgres`) and `RedisSiteDraftStore` (feature `redis`) keep a site's drafts
  and requests for every replica of a served site. Ids come from a shared per-site,
  per-kind counter and every write is a compare-and-swap on the record's version; both
  pass `site_draft_store_conformance_tests!` in CI's postgres job.
