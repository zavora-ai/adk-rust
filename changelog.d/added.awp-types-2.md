- **AWP 2.0 wire types** (`awp-types`, spec 3.5 §7.2–§7.8.3, §7.10): `awp_types::v2` holds the
  signed site contract (`SiteContract`), keys, delegations and revocations (`KeySet`,
  `Delegation`, `Revocation`, `Proof`), identity, trust tiers and trust rules (`IdentityPolicy`,
  `TrustTier`, `TrustRules`, `RequesterId`), typed capabilities (`CapabilityV2`,
  `CapabilityManifestV2`, `MachineView`), held calls (`HeldCall`), Standard Webhooks events and
  subscriptions (`EventEnvelope`, `EventSubscription`, `CatalogChangesPage`), site status
  (`SiteStatus`), consent receipts (`ConsentReceipt`), commerce receipts and quotes (`Receipt`,
  `QuoteDocument`), the error envelope (`ErrorEnvelope`), release manifests with snapshot and
  delta forms and site log entries (`ReleaseManifest`, `LogEntry`), stream entries
  (`StreamEntry`), the charter (`Charter`, `SiteKind`, `Governance`, `Kpi`, `Guardrail`,
  `AutonomyLevel`, `PublicTerms`, `PriceRule`), signals (`Signal`, `Bounded`) and the content and
  batching types (`ContentItem`, `BatchPolicy`, `FeedBounds`), all re-exported at the crate root
  beside the unchanged 1.0 names. `Charter::validate()` refuses a KPI target without a baseline,
  a charter without an owner, an agent holding a role, and an approval chain distinct principals
  cannot satisfy. The new `json-schema` feature derives JSON Schema (2020-12) for every 2.0 type;
  the generated schemas are committed under `awp-types/schemas/2.0/`.
