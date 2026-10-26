- **The owner's design choice gates migration parity** (`adk-site`, `adk-website`,
  `adk-cli`): `ParityConfig::design` takes `SiteDesign::Keep` (the perceptual difference
  under 0.05 gates the launch) or `SiteDesign::NewTheme` (URL coverage, text and metadata
  gate it; the look is reported and the owner approves it from the preview).
  `record_design_choice` writes the choice to the migration audit as
  `site.migration.decided`; `adk-rust site migrate --design keep|new-theme` records it in
  the migration report (spec 3.5 §7.8.1).
