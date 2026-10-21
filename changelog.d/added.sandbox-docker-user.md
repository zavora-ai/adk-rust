- **`DockerClient::with_user`** (`adk-sandbox`, `workspace-docker`): runs every container
  the client creates as a given `uid`, `uid:gid` or user name, so files a command writes into
  a workspace mounted with `with_bind_mount` belong to the host user who mounted it rather
  than to root. Spec 3.1 §10 S4.
