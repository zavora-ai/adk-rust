- **The server is reachable from this machine only by default** (`adk-server`, `adk-cli`,
  `cargo-adk`): a page on another origin, or on a DNS-rebinding domain, could run the agent and
  read any user's sessions.
  - `Launcher` serve mode, `adk_cli::serve::run_serve`, and `adk-rust serve` listen on
    `127.0.0.1`. `Launcher::with_bind_address` and `serve --host 0.0.0.0` listen elsewhere. The
    `cargo-adk` REST template and `server` addon read `BIND_HOST` (default `127.0.0.1`); the
    generated Dockerfiles set `BIND_HOST=0.0.0.0`.
  - An empty `SecurityConfig::allowed_origins` allows no cross-origin access: preflights get no
    `Access-Control-Allow-Origin`, and a request whose `Origin` is another origin is refused with
    `403`. `*` allows every origin; `http://localhost:*` allows any port.
  - New `SecurityConfig::allowed_hosts`: a request whose `Host` is not `localhost`,
    `*.localhost`, an IP address, the A2A base URL's host, the backend URL's host, or a listed
    name is refused with `403`. Agent Engine routes skip the check.
  - `SecurityConfig::development()` allows loopback origins on any port instead of every origin.
  - `SecurityConfig` gains a public `allowed_hosts` field, so a struct literal must add it;
    `A2aServerBuilder::security_config` sets the configuration of an `A2aServer`.

  Migration: where the server must be reachable from other machines, listen with
  `with_bind_address` or `--host`, list its public host names with
  `ServerConfig::with_allowed_hosts`, and list the browser origins that call it with
  `with_allowed_origins` (`vec!["*".to_string()]` restores the previous CORS behaviour).
