- **gVisor, read-only roots and remote daemons for `DockerClient`** (`adk-sandbox`,
  `workspace-docker`): `with_runtime("runsc")` runs every container under an OCI runtime
  the daemon knows, such as gVisor. `with_read_only_root(ReadOnlyRoot { .. })` makes the
  image read-only and gives the container size-capped `nosuid,nodev` tmpfs mounts for
  `/workspace` and a `noexec` `/tmp`; a bind mount at `/workspace` takes the place of its
  tmpfs. `DockerClient::connect_with_defaults` honours `DOCKER_HOST` (and
  `DOCKER_TLS_VERIFY`/`DOCKER_CERT_PATH`), so containers can run on a separate sandbox host
  that holds no keys.
