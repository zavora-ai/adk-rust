- **Deploy manifests accept `secretRef` in `[env]`** (`adk-deploy`): an environment entry
  that referenced a secret parsed only when written `{ secret_ref = "KEY" }`, while the rest
  of the manifest, and the README's example, use camelCase, so `{ secretRef = "KEY" }` failed
  to load. Both spellings now parse, and a manifest is written with `secretRef`.
