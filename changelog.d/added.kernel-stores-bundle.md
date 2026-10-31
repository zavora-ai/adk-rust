- **One bundle of every kernel store, and kernels that refuse to forget** (`adk-kernel`,
  `adk-cli`, `adk-website`): `adk_kernel::stores::KernelStores` holds every stateful store a
  kernel keeps — runs, schedules, approvals, action and spend ledgers, the kill switch,
  processes, agenda, partitions, cron watermarks, added agents, devices, control settings,
  drivers, the activity ledger, chat pairings and the charter stores (`charter::CharterStores`)
  — opened together with `KernelStores::open(url)` on `memory:`, SQLite, PostgreSQL, Redis,
  MongoDB, Neo4j or Firestore, or from a pool or client (`from_sqlite_pool`,
  `from_postgres_pool`, `from_redis_client`, `from_mongo_database`, `from_neo4j_graph`,
  `from_firestore_db`). Devices, control settings, chat pairings and the charter stores have no
  MongoDB, Firestore or Neo4j backend yet; a bundle there keeps them in memory and names them
  (`in_memory_stores`). `KernelBuilder::stores` and `TenantKernelBuilder::stores` take the
  bundle, a store set on the builder winning, and mirror the kill switch over its governance
  store. The builder now logs a warning for each stateful store it keeps in memory because none
  was given; `KernelBuilder::require_durable()` refuses to build instead, naming each store and
  the setter that fixes it, and `Kernel::store_uses()` reports where every store came from.
  `adk-rust up` (unless `--ephemeral`) and `adk-website serve` take the bundle and require
  durable stores, so the served site's kill switch and control-plane settings now survive a
  restart too. `adk-website`'s store bundle is `deploy::SiteStores`, with the kernel's under
  `kernel`. `adk-kernel`'s `redis` feature turns on `adk-session/redis`.
