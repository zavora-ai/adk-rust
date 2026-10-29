- **`SystemTxtLookup` reads the system's DNS settings at its first lookup** (`adk-drivers`
  `system-dns`, spec 3.5 §7.13): `SystemTxtLookup::new` no longer fails when the resolver
  configuration cannot be read, so a served site configures its email whatever the system's
  DNS settings are at that moment; a lookup that cannot read them returns
  `DriverError::Unavailable` and the next one reads them again.
