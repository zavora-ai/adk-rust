- **`harness.skill.activated` audit record** (`adk-harness`): a skill loaded through
  `load_skill` records its id, content hash, source path, allowed tools and the function
  call id through the run's audit trail. `load_skill` keeps the skill toolset's exact
  declaration (spec 3.1 §9.3).
