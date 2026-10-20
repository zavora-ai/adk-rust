- **`DockerClient::with_bind_mount`** (`adk-sandbox`, `workspace-docker`): mounts a host
  path into every container the client creates, read-write or read-only, so commands in
  the container and file tools on the host share one view of a workspace. Mounts are
  passed as structured Docker mounts, so a `:` in either path is not read as an option;
  a host path that is not valid UTF-8 fails provisioning with `ProvisionFailed`.
  Spec 3.1 §10 S4.
