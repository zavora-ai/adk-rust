- **Site pipeline wave fixes** (`adk-site`, spec 3.5 §7.8, §7.8.1, §7.8.3):
  - `Detectors::scan_cloaking` runs the dual-fetch cloaking check over an inventory's top
    pages for a proven domain; `adk-website` discovery and `adk-rust site migrate` run it.
  - `Presence` reads any schema.org subtype of an organisation, business, person or place
    of worship (`schema_org_ancestry`) and records the subject's `@type` fact.
  - `LaunchGate::with_reviews` polices review quotes: a page quoting an anonymous,
    unapproved or unnamed review fails the claims check.
  - `GateCheck::ThirdPartyResources` and `GateCheck::Signing`, with the
    `ThirdPartyResources` (scripts, frames, images, media, stylesheets, fonts and CSS
    imports from another origin) and `ReleaseDelegation` extensions, moved from
    `adk-website`, which re-exports them.
  - The built-in generator writes a product feed at `products/feed.xml` per language.
  - `UpdateEngine` keeps held items and deliveries in a `HeldStore`; `FileHeldStore`
    survives restarts.
