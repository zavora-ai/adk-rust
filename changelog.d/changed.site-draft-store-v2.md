- **`SiteDraftStore` allocates ids and writes by version** (`adk-harness`): the trait's
  `put` and `list` are replaced by `create` (a store-allocated id such as `draft-7`, at
  version 1), `get`, `replace` (a compare-and-swap on the version), `remove` and `list`,
  which returns `StoredRecord`s in creation order. Each kind numbers its own ids, so the
  first charter draft is `charter-1` whatever was drafted before it. `SiteRuntime` reads
  its drafts from the store at every call, so runtimes over one store share them; a lost
  compare-and-swap is `website.drafts.conflict`. `SqliteSiteDraftStore::migrate` upgrades
  an earlier table in place. `conformance::site_draft_store` gives way to the
  `site_draft_store_conformance_tests!` macro over a `SiteDraftStoreHarness`.
