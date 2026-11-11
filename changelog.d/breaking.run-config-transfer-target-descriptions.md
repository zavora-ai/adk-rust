- **`RunConfig` gains `transfer_target_descriptions`** (`adk-core`): the runner fills it with
  the description of each transfer target, by name, for the agent's transfer guidance.
  Code that builds or destructures `RunConfig` field by field must add it.

  Migration: add `transfer_target_descriptions: HashMap::new()` to struct literals, or
  build from `RunConfig::default()` or the builder.
