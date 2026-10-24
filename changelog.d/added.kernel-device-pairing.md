- **Device pairing for the agent kernel** (`adk-kernel`, `adk-cli`; spec 3.3 §10 K2): a
  phone, a tray or a remote terminal reaches the control plane with a device token instead of
  the operator's credentials. `adk-rust pair` issues a ten-minute, twelve-character code; the
  device claims it at `POST /api/kernel/pairing/claim` and receives `adkd_<id>_<secret>`
  once. The token opens only the routes its scopes name — `read`, `decide` (low and medium
  risk), `stop` — never `unfreeze`, and on loopback it is the device, never the operator.
  Codes and tokens are stored as SHA-256 hashes in the `DeviceStore` (in-memory, SQLite on
  `harness.db`, PostgreSQL, Redis, with `device_store_conformance_tests!`), which also counts
  claims for every replica: twenty without a success cancel every waiting code. Devices
  expire after 90 days by default (1 to 365); `adk-rust pair revoke` removes one at once and
  ends the event stream it holds open. `adk-rust pair list` shows the paired devices and chat
  accounts. Pairing never widens the bind. `ControlPlaneConfig::with_pairing`,
  `pairing::DeviceTokenExtractor`, `control::guard_devices` and `control::claim_router` wire
  it into a server; every pairing action writes a `kernel.control` record before it takes
  effect.
