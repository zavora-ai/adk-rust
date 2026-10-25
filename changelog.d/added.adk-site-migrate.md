- **`adk-site` migration** (feature `migrate`, spec 3.5 §7.8, §7.8.1, §7.18, A-I9, A-I10,
  Experimental): `SiteSource` implementations `WordPressRest` (public collections; drafts with a
  read-scoped application password from the `CredentialBroker`), `Wxr`, `Crawl`, `Presence`
  (every fact `Assumed`) and `IsolatedReplay` (a dump and uploads on an internal container
  network, loopback-only, destroyed afterwards, zero outbound connections recorded); one
  `HttpClient` that signs every crawler request with the site's Web Bot Auth key, obeys
  `robots.txt`, rate-limits per origin and records every request; `DomainClaim` by DNS TXT or
  well-known token, without which only polite public reads run; an allowlist `Sanitiser` from
  HTML to Markdown; `Detectors` for hidden-link spam, cloaking, executables and MIME
  mismatches among uploads and obfuscated scripts, whose findings a `QuarantineBook` holds
  until a per-item `ApprovalGate` decision; `map_features` from legacy features to
  `CapabilityV2` drafts and owner decisions; `Freeze`, a private-vantage crawl published as a
  signed release with forms posting to edge capabilities; `MigrationParity`, the launch gate's
  migration rows with headless-Chrome screenshots; `Prober`, drift against the release window
  for the Guardian; and `LegacyArchive`, the legacy export encrypted through `adk-artifact`.
