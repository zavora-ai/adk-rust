- **`DockerClient::provision` removes its container when provisioning fails** (`adk-sandbox`,
  `workspace-docker`): a manifest entry that cannot be written or cloned no longer leaves the
  container it was created for running with no handle to stop it.
