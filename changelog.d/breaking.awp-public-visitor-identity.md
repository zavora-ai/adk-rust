- **AWP visitor ids changed, and `RunConfig` carries unauthenticated request metadata**
  (`adk-server`, `adk-core`, `adk-runner`): sessions kept under the earlier `public-…` ids
  are no longer reached, so returning visitors start new conversations; keep
  `ConsentConfig::subject_key` across restarts to keep visitor ids stable. The new
  `RunConfig::request_metadata` field (and `RunConfigBuilder::request_metadata`) passes
  metadata to tools through `request_metadata()` without authenticating anyone; a
  `RunConfig` struct literal needs `..RunConfig::default()`.
