- **Capability packs as signed bundles** (`adk-website`, `adk-cli`): a pack is one JSON
  document (`adk-website.pack/1`) of capability declarations, the configuration of the built-in
  handlers that serve them, playbooks and driver manifests, sealed with a content digest and its
  publisher's AWP 2.0 detached JWS (`packs::bundle`). `adk-rust site packs install` verifies a
  bundle against the publisher keys in the project's `packs/trust.json` and refuses one that is
  unsigned, tampered, from an unknown publisher, invalid, in conflict with the site's packs, a
  downgrade or a republished version; `list`, `remove`, `trust` and `sign` complete the set. A
  served site verifies its installed packs on every start, serves their capabilities on the edge
  (`SiteEdge::build_with_packs`) and writes their playbooks into its agents' workspaces. The six
  default packs ship as signed bundles; conformance vectors are in
  `adk-website/tests/fixtures/packs/` (spec 3.5 §7.23).
