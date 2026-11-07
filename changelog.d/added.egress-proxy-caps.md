- **Byte and lifetime caps on egress proxy connections** (`adk-sandbox`, feature
  `egress-proxy`): `EgressProxyBuilder::max_bytes_per_direction` and
  `max_connection_lifetime` close an admitted connection that carries too much or stays
  open too long, so a sandbox cannot turn an admitted host into a bulk transfer channel or
  hold an idle tunnel open. Reaching the byte cap in either direction cuts the whole
  connection, and the ClientHello or request head counts toward it. Such connections are
  recorded as the new `EgressOutcome::Capped { reason }`; `EgressOutcome` is now
  `#[non_exhaustive]`. Byte counts in `EgressRecord` are now kept as bytes flow, so they
  stay exact when a connection ends with an error or at a cap.
