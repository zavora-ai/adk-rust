- **Device kinds, server routes for devices and device attachments** (`adk-kernel`; spec 3.3
  §7.2, §10 K2): a pairing code carries the kind of the device it pairs — `DeviceKind::Phone`,
  `Terminal` or `Tray`, set by the issuer (`IssueCode::with_kind`, `IssueRequest.kind`,
  `adk-rust pair --kind`) and never by the claim — and a device's requests carry it in
  `DEVICE_KIND_METADATA`; a device paired without one is a remote terminal.
  `Pairing::with_device_routes` opens a server's own routes to devices after the kernel's
  `device_scope_for`, and the device guard checks the risk of, and records, the kernel's own
  decide route only. `DeviceStore` keeps `DeviceAttachment`s beside a device or a person
  (`put_attachment`, `put_attachment_if_absent`, `attachments`, `remove_attachment`,
  `remove_attachments`) on the in-memory, SQLite, PostgreSQL and Redis stores, with
  conformance cases; revoking a device removes them. `Pairing::until_device_ends` ends a
  device's open streams with it, and `Kernel::quiet_until` gives the end of the quiet time
  holding a moment in an area, with changes made while the kernel runs.
