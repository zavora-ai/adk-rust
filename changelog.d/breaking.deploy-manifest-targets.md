- **`DeploymentManifest` has a `targets` field** (`adk-deploy`): one `[targets.<name>]` table per
  deploy target. A struct literal naming every field adds `targets` or ends with
  `..DeploymentManifest::default()`.
