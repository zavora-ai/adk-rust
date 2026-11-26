- **Workspace vision rules apply as written** (`adk-harness`): the vision pack no longer
  turns a workspace `.adk/vision.toml`'s `allow` rules and default into `ask`.
  `VisionPack::distrust_workspace_rules` replaces `trust_workspace_rules` as the opt-in to
  that restriction, and `load_vision_config`'s flag now distrusts. ReadOnly and Plan modes
  deny every vision call that sends an image off the machine, whatever the rules allow,
  through the new `CallClasses::denial_in_mode`.
