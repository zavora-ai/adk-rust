- **The server is reachable from this machine only by default** (`adk-server`, `adk-cli`,
  `cargo-adk`): a page on another origin, or on a DNS-rebinding domain, could run the agent and
  read any user's sessions.
  - `Launcher` serve mode, `adk_cli::serve::run_serve`, and `adk-rust serve` listen on
    `127.0.0.1` instead of `0.0.0.0`. `Launcher::with_bind_address` and `serve --host 0.0.0.0`
    listen on other interfaces; `run_serve` always listens on loopback. The `cargo-adk` `api`
    and `server` addons read `BIND_HOST` (default `127.0.0.1`), as the `api` and `a2a`
    templates already did, and the generated Dockerfiles set `BIND_HOST=0.0.0.0`.
  - An empty `SecurityConfig::allowed_origins`, the default, allows no cross-origin access
    instead of every origin: preflights get no `Access-Control-Allow-Origin`, and a request
    whose `Origin` names another origin is refused with `403`. `*` allows every origin;
    `http://localhost:*` allows any port. `SecurityConfig::development()` allows loopback
    origins on any port instead of every origin.
  - The new public field `SecurityConfig::allowed_hosts` (struct literals must add it) lists
    the host names the server answers besides `localhost`, `*.localhost`, IP addresses, and
    the hosts of the A2A base URL and the backend URL; a request naming any other `Host` is
    refused with `403`. Agent Engine routes skip the host check.
    `A2aServerBuilder::security_config` sets the configuration of an `A2aServer`.

  Migration: where the server must be reachable from other machines, listen with
  `with_bind_address` or `--host`, list its public host names with
  `ServerConfig::with_allowed_hosts`, and list the browser origins that call it with
  `with_allowed_origins` (`vec!["*".to_string()]` restores the previous CORS behaviour).
