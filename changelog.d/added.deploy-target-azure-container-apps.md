- **Azure Container Apps deploy target** (`adk-deploy`, `cargo-adk`): feature `azure` adds
  `azure::ContainerAppsClient`, which creates or updates, reads and deletes
  `Microsoft.App/containerApps` through Azure Resource Manager with `DefaultAzureCredential` or
  a supplied token, and `azure::ContainerAppsTarget`, behind
  `cargo adk deploy --target azure-container-apps`. Secrets are Key Vault references resolved by
  a user-assigned managed identity, `[health]` becomes liveness and readiness probes, and
  `[scaling]` the replica bounds with an HTTP concurrency rule.
