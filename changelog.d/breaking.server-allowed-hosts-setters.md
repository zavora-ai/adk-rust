- **Both `with_allowed_hosts` setters replace the host list** (`adk-server`):
  `SecurityConfig::with_allowed_hosts` added to the list while
  `ServerConfig::with_allowed_hosts` replaced it, so the same call answered different hosts
  depending on which type it was made on. Both now replace the list set before, and
  `ServerConfig::with_allowed_hosts` takes any `IntoIterator` of `Into<String>`, as
  `SecurityConfig::with_allowed_hosts` does; a `Vec<String>` argument still compiles. Pass
  every host in one call where a second call relied on adding to the first.
