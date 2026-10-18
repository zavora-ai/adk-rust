- **`ContainerConfig` and `DockerConfig` gain `drop_capabilities`, `no_new_privileges`
  and `pids_limit`** (`adk-code`): all three are on by default (`true`, `true`,
  `Some(DEFAULT_PIDS_LIMIT)`); struct literals need the fields or
  `..ContainerConfig::default()`. Setup commands that switch users, such as `apt-get`,
  need `drop_capabilities: false`.
