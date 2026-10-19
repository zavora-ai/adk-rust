- **`allowed-tools` may be one string** (`adk-skill`): `allowed-tools: Read Grep`, as
  the `agentskills.io` specification writes it, and comma-separated names parse as a
  list. A scalar value failed the frontmatter, and the skill was dropped from the index
  without a warning; a file with frontmatter that does not parse is now skipped with a
  `warn!`.
