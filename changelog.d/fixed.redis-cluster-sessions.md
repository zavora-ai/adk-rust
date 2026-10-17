- **`RedisSessionService` works on Redis Cluster** (`adk-session`): with `cluster_nodes` or a
  `redis-cluster://` URL, every `create` and append failed (`CROSSSLOT`, or fred's "same cluster
  node" error) because one script or transaction named keys in several slots. Cluster
  connections now keep each session's hash and event set under one hash tag
  (`adk:session:{<app>:<user>:<session>}`), with `{` and `}` in identifiers percent-encoded, and
  write the shared app and user state tiers as separate commands after the session's script. A
  reader can briefly see an appended event before its app and user tier fields. Standalone
  servers keep their key names and single-step writes, so existing sessions stay readable.
