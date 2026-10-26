# Site migration

`adk-site`'s `migrate` feature moves an existing website or online presence onto a static,
signed AWP 2.0 site (spec 3.5 §7.8, §7.8.1, §7.18). It inventories the old site, leaves its
infection behind, maps its dynamic features to agent-edge capabilities, can freeze it in
hours, checks migration parity for the launch gate, probes the live site for drift, and
archives the legacy export encrypted.

> **Note:** `adk-site` is **Experimental**. Its API may change while spec 3.5 lands.

```toml
[dependencies]
adk-site = { version = "3.0.0", features = ["migrate"] }
```

`adk-rust site migrate` and `adk-rust site freeze` run these stages on a site project from the
command line ([Sites and AWP from the command line](site-cli.md#migration-and-freeze)).

## Stages

| Stage | Type | Behaviour |
|-------|------|-----------|
| Domain claim | `DomainClaim`, `ProvenDomain`, `ClaimScope` | Proof of control by a TXT record at `_awp-claim.<domain>` or a token at `/.well-known/awp/claim/<token>`; until proven only polite public reads run (A-I9) |
| Outbound HTTP | `HttpClient`, `CrawlSigner` | Every crawler request signed with the site's Web Bot Auth key, `robots.txt` read first and obeyed, one request at a time per origin, egress checked and recorded |
| Sources | `WordPressRest`, `Wxr`, `Crawl`, `Presence`, `IsolatedReplay` | Each produces a `SiteInventory`: URLs, items, media, taxonomies, menus, dynamic features, facts and reviews |
| Detectors | `Detectors` | Hidden-link spam (hidden elements plus `adk-guardrail`'s `ContentFilter`), cloaking (a crawler fetch and a browser fetch compared), executables and MIME mismatches among uploads, obfuscated scripts |
| Quarantine | `QuarantineBook` | Every finding holds its item or file out, with evidence, until a per-item `ApprovalGate` decision; server executables are never released (A-I1) |
| Sanitiser | `Sanitiser` | HTML to Markdown through an allowlist; scripts, inline handlers, `<object>`, `<embed>`, unlisted `<iframe>` and hidden elements dropped and recorded |
| Content | `convert::to_content` | Items keep their URLs through the `path` field; uploads keep their paths; archives, feeds and legacy redirects become 301s |
| Feature mapping | `mapping::map_features` | `CapabilityV2` drafts and named owner decisions |
| Freeze | `Freeze` | A crawl from a private vantage point published as a signed release; forms post to edge capabilities; the old server is never contacted at request time |
| Parity | `MigrationParity` | The launch gate's migration rows: URL coverage, main-text similarity of 0.95 or more, metadata preserved, and — when the owner keeps the design — a perceptual difference under 0.05 at two widths |
| Probes | `Prober` | Sampled digests on the host and at the origin against the release window, status, certificate expiry, DNS records, security headers; `Drift` for the Guardian |
| Archive | `LegacyArchive` | The legacy export stored AES-256-GCM encrypted through `adk-artifact`; storing appends a version and nothing deletes (A-I10) |

## Sources

| Source | Reads | Needs |
|--------|-------|-------|
| `WordPressRest` | `wp-json/wp/v2` posts, pages, media, categories and tags; WooCommerce's Store API products; drafts and menus with a read-scoped application password from the `CredentialBroker` | Drafts: a proven domain and a credential whose only scope is `read` (A-I19) |
| `Wxr` | A WordPress export file; with `with_uploads`, every file in the uploads directory, also files no attachment names | Nothing |
| `Crawl` | Any site, from its sitemaps and links, same origin only | Nothing |
| `Presence` | Business profiles, listings, reviews and social pages: schema.org JSON-LD and Open Graph tags; the subject is the most specific organisation, business, person or place of worship on the page, any schema.org subtype such as `Bakery` included, and its type is the `@type` fact | Nothing |
| `IsolatedReplay` | A database dump and uploads served by containers on an internal network, published on `127.0.0.1` only, crawled, then destroyed | A proven domain and a `ContainerDriver` (`DockerCliDriver` runs the Docker CLI) |

Every fact a source finds is `Assumed` and names the page and property it came from.
`SiteInventory::fact_table` turns them into a claim check, so a page that cites one fails the
launch gate until the owner confirms it (A-I18). A review quote is usable only with the
reviewer's public name and the owner's approval (`ReviewQuote::usable`); the launch gate
enforces it: given the reviews (`LaunchGate::with_reviews`, a `QuotableReview` from each
`ReviewQuote`), the claims check holds a page that quotes one — found by its words, whatever
the markup — when the review is anonymous (`review-quote-anonymous`), not approved
(`review-quote-unapproved`) or not named on the page (`review-quote-unattributed`).
`schema_org_ancestry` gives a type's parents, so kind detection reads a `Bakery` as a
`LocalBusiness`.

The isolated replay records every request its crawler made; `ReplayReport::outbound_connections`
lists those that went anywhere but loopback and is empty, and the replay refuses its result
otherwise. The dump and the uploads are mounted read-only and never removed.

## Proving a domain and crawling

```rust
use std::sync::Arc;

use adk_auth::web_bot_auth::{Ed25519SigningKey, HttpSignatureSigner};
use adk_core::{ActingContext, SubjectKind};
use adk_site::migrate::claim::{DomainClaim, SystemDnsResolver};
use adk_site::migrate::http::HttpClient;
use adk_site::migrate::sources::Crawl;
use adk_site::migrate::{ClaimScope, SiteSource, SourceContext};

async fn inventory() -> adk_site::migrate::Result<()> {
    // The site's Web Bot Auth key signs every crawler request.
    let key = Arc::new(Ed25519SigningKey::generate().expect("key"));
    let signer = HttpSignatureSigner::builder(key, "https://example.com").build().expect("signer");
    let http = Arc::new(HttpClient::builder().signer(Arc::new(signer)).build()?);
    let acting = ActingContext::derived("example.com", "migrator", "owner", SubjectKind::Human);
    let mut ctx = SourceContext::new(acting, http);

    // The owner publishes the TXT record; until it verifies, only polite reads run.
    let claim = DomainClaim::new("example.com", chrono::Utc::now())?;
    let (name, value) = claim.dns_record();
    println!("publish TXT {name} \"{value}\"");
    if let Ok(proven) = claim.verify_dns(&SystemDnsResolver, chrono::Utc::now()).await {
        ctx = ctx.with_scope(ClaimScope::Proven(proven));
    }

    let inventory = Crawl::new("https://example.com/".parse().unwrap()).inventory(&ctx).await?;
    println!("{} URLs, {} items", inventory.urls.len(), inventory.items.len());
    Ok(())
}
```

## Leaving the infection behind

Detect, convert, quarantine against the conversion, then convert again without the held
subjects. A decision that releases a subject is followed by another conversion.

```rust
use adk_site::migrate::convert::{ConvertOptions, to_content};
use adk_site::migrate::detect::Detectors;
use adk_site::migrate::mapping::map_features;
use adk_site::migrate::quarantine::QuarantineBook;
use adk_site::migrate::{SiteInventory, SourceContext};

async fn migrate(ctx: &SourceContext, inventory: &SiteInventory) -> adk_site::migrate::Result<()> {
    let detectors = Detectors::new();
    let mut findings = detectors.scan_inventory(inventory).await;
    // The dual fetch reads the live site: it runs for a proven domain only (A-I9).
    findings.extend(detectors.scan_cloaking(ctx, inventory, 20).await.findings);
    let options = ConvertOptions::new("en".parse().unwrap());
    let first = to_content(inventory, &options)?;
    let book = QuarantineBook::new(findings, &first);
    let converted = to_content(inventory, &options.clone().excluding(book.excluded()))?;
    let features = map_features(inventory);
    println!(
        "{} items, {} quarantined, {} capabilities, {} owner decisions",
        converted.items.len(),
        book.entries.len(),
        features.capabilities.len(),
        features.decisions.len()
    );
    Ok(())
}
```

| Finding | Subject held | Release |
|---------|--------------|---------|
| `HiddenLinks`, `SpamLinks` | The item | A per-item decision |
| `Cloaking` | The page's item | A per-item decision |
| `ObfuscatedScript` | The item or script | A per-item decision |
| `MimeMismatch` | The upload | A per-item decision |
| `Executable` | The upload | Never (A-I1) |

`QuarantineBook::decide` asks the `ApprovalGate` about one item, bound to the digest of its
evidence, under the action class `site.migration.release_quarantined`; an approval by the
proposer keeps the item held. `QuarantineBook::quarantine` is the launch gate's `Quarantine`.

`Detectors::scan_cloaking` fetches the home page and the top pages twice, as the crawler and
as a browser, and returns a `CloakingScan` with the findings, the pages checked and notes; for
a domain the owner has not proved it fetches nothing and says why. `adk-website`'s
`Discovery::run` and `adk-rust site migrate` run it.

After a crawl (`Crawl`, `IsolatedReplay`) the converter turns a link to a page of the site the
crawl could not read — one `robots.txt` disallows, say — into its text, records a
`DropReason::UnreadTarget` and a note, so the gate's link check does not hold the release on
a page the new site never had. Legacy runtime paths the new site does not serve answer 410
Gone: anything under `LEGACY_RUNTIME_PREFIXES` (`/wp-includes/`, theme and plugin files,
`/wp-json/`) or ending in a server-script extension, and after a WordPress migration
`WORDPRESS_ENDPOINTS` such as `/wp-login.php` and `/xmlrpc.php`; a page the crawl read keeps
its 301 (A-I4).

## Feature mapping

| Legacy feature | Becomes |
|----------------|---------|
| Contact form | `submit_inquiry`: `NonIdempotent`, consent scope `contact`, the form's fields as the input schema |
| Search | A static index and `search_site`: `ReadOnly` |
| Comments | An owner decision, off by default; `FeatureMap::answer(Comments, "held")` adds `submit_comment` with every call held |
| Shop | Product items, `get_quote` and `place_order` (`known` tier; `place_order` `NonIdempotent`) |
| Bookings | `check_availability` and `book` |
| Logins and member areas | The owner decision `migration.accounts`: remove, link to a named service, or leave out |
| Newsletter sign-up | `subscribe_newsletter`, consent scope `newsletter` |

## Freeze

`Freeze::run` crawls the legacy server through a client that reaches only the vantage origin
(the public origin mapped onto it), writes the snapshot, leaves out server code and
quarantined files, rewrites each mapped form to post to `<edge>/awp/v2/capabilities/<name>`,
releases it into the site log as `ReleaseSource::Migration` and publishes it to the host.
Serving the release reads only the host's files.

## Parity

`MigrationParity` is a launch-gate extension for the full tier. URL coverage exempts refused
and quarantined subjects; text and metadata compare the top content pages; the visual
comparison needs a `Screenshotter` and a legacy view (the live site, a replay or a frozen
snapshot) and is reported as not measured without them. `ChromeScreenshotter` runs headless
Chrome; `perceptual_difference` is one minus `image-compare`'s hybrid score.

The owner chooses the design during migration, and `ParityConfig::design` carries the choice:

| `SiteDesign` | When | Gates the launch | Looks |
|--------------|------|------------------|-------|
| `Keep` (default) | A freeze, or the old design carried over | URL coverage, main text, metadata, perceptual difference under 0.05 | Gated |
| `NewTheme` | A new theme | URL coverage, main text, metadata | Reported as non-blocking findings; the owner approves the look from the preview |

`MigrationParity::with_design` sets it; `record_design_choice` writes it to the migration
audit as `site.migration.decided` with `design_choice_fields`. `adk-rust site migrate
--design keep|new-theme` records it in the migration report.

> **Note:** a migration onto a new theme changes how pages look. Measured with Chrome on the
> recorded WordPress fixture, the built-in theme differs from the legacy theme by up to 0.17,
> which is why a new theme's looks are approved by the owner rather than gated.

## Probes

`Prober::new` needs a `ProvenDomain` (A-I9). Each probe reads the host's checkpoint, verifies
the release in the site log, samples files, and compares their digests on the host and at the
public origin with the release the checkpoint names or the one after it (§7.8.3). A file no
release explains, a missing file, a served legacy runtime path such as `/wp-login.php`, or a
checkpoint that does not verify is a `Drift`; `site.drift.detected` is recorded when an audit
recorder is set.

## Archive

```rust
use std::sync::Arc;

use adk_artifact::InMemoryArtifactService;
use adk_site::migrate::archive::{ArchiveKey, LegacyArchive};

async fn archive(export: &[u8]) -> adk_site::migrate::Result<()> {
    let archive = LegacyArchive::new(
        Arc::new(InMemoryArtifactService::new()),
        "example.com",
        "owner",
        "migration",
        ArchiveKey::generate()?,
    );
    let receipt = archive.store("export.xml", export, chrono::Utc::now(), None).await?;
    let restored = archive.load(&receipt.file_name, Some(receipt.version)).await?;
    assert_eq!(restored.as_ref(), export);
    Ok(())
}
```

## Audit records

| Record | Written by |
|--------|-----------|
| `site.migration.finding` | `QuarantineBook::record_findings` |
| `site.migration.decided` | `QuarantineBook::decide` with an audit recorder |
| `site.freeze` | `Freeze::run` with `SourceContext::with_audit` |
| `site.archive` | `LegacyArchive::store` with an audit recorder |
| `site.drift.detected` | `Prober::probe` with `Prober::with_audit` |
