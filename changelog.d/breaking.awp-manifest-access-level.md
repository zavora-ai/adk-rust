- **The AWP 1.x capability manifest carries access levels and schemas** (`awp-types`,
  `adk-awp`, spec 3.5 §10 X1): `build_manifest` and `GET /awp/manifest` drop neither any more.
  `CapabilityEntry` gains `access_level: Option<TrustLevel>` (`accessLevel` on the wire,
  omitted when absent), and `BusinessCapability` gains optional `input_schema` and
  `output_schema`, read from `business.toml`. The wire change is additive; struct literals of
  either type must add the new fields.

  Migration: add `access_level: None` to `CapabilityEntry` literals and
  `input_schema: None, output_schema: None` to `BusinessCapability` literals.
- **`business_context_to_toml` reports `AwpConfigError::Serialize`** (`adk-awp`, spec 3.5 §10
  X4): a context TOML cannot represent, such as a product price above `i64::MAX`, returned
  `AwpConfigError::WatcherError`; it returns the new `Serialize` variant with the `toml` error
  as its source.

  Migration: match `AwpConfigError::Serialize` where serialization failures were handled as
  `WatcherError`.
