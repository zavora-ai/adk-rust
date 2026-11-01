- **Egress proxy for sandboxed processes** (`adk-sandbox`, feature `egress-proxy`):
  `egress_proxy::EgressProxy` admits a child process's `CONNECT` tunnels and absolute-form
  `http` requests by an `adk_core::egress::EgressPolicy`. It resolves names itself and connects
  only to an address it checked, requires the TLS server name to match the `CONNECT` host
  (refusing no SNI, Encrypted Client Hello and non-TLS streams), never connects to loopback,
  answers refusals with `403` and an `X-Adk-Egress-Denied` code, and files one `EgressRecord`
  per connection with the call id, destination, SNI, outcome and byte counts. Refs #1281.
