- **`ContainerConfig` and `DockerConfig` gain `drop_capabilities`, `no_new_privileges`,
  and `pids_limit`** (`adk-code`): the defaults and presets set `true`, `true`, and
  `Some(DEFAULT_PIDS_LIMIT)` (256). Struct literals need the three fields or
  `..ContainerConfig::default()` / `..DockerConfig::default()`. `DockerConfig` setup commands
  that switch users, such as `apt-get`, need `drop_capabilities: false`.
