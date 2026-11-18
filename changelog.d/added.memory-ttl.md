- **Memory expiry and delete by id on every backend** (`adk-memory`): an entry expires at
  `MemoryEntry::expires_at`, set per entry with `with_ttl` or per service with
  `with_default_ttl`. Search and `list_recent` never return an expired entry on any of the
  eight backends; `MemoryService::purge_expired` removes them, and
  `expiry::spawn_expiry_sweep` runs it on an interval. Redis list keys and a MongoDB TTL
  index expire entries natively, and Memory Bank memories take the expiry as their
  `expireTime`. `MemoryService::delete_memory` removes one entry by the id every returned
  entry carries. The `conformance` feature's `memory_conformance_tests!` holds a backend to
  the contract.
