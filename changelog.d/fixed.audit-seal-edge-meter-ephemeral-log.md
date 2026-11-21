- **`EdgeMeter` and `EphemeralAuditLog` seal** (`adk-website`, `adk-harness`): `EdgeMeter`
  forwards `AuditRecorder::seal` to the recorder it wraps, and `EphemeralAuditLog` signs a
  checkpoint over its unsigned records, so a signed log behind either verifies after a clean
  stop.
