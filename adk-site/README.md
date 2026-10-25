# adk-site

The site pipeline of an AWP 2.0 website (spec 3.5 §7.8): content as files, a static
generator that writes a Markdown and a structured twin of every page, a launch gate, signed
releases in an append-only log, and hosts that switch releases atomically. No code runs when a
page is requested: a site is static files plus a log that proves which files are the site.

> **Note:** `adk-site` is **Experimental** (see `STABILITY.md`). Its API may change without
> notice while spec 3.5 lands.

```toml
[dependencies]
adk-site = "3.0.0"
```

## What it does

| Stage | Type | Behaviour |
|-------|------|-----------|
| Content | `SiteProject`, `ContentStore`, `ContentHistory` | Items are Markdown files with front matter; drafts stay apart until released; every version is recorded |
| Generator | `SiteGenerator`, `BuiltinGenerator` | Pages per language, `index.md` and `index.awp.json` twins linked with `<link rel="alternate">`, `llms.txt`, `robots.txt` with `Content-Usage`, `security.txt`, sitemaps with `lastmod`, Atom feeds, schema.org JSON-LD per kind, redirects in the host's format; identical inputs give identical files; incremental builds rebuild an item and its dependents only |
| Launch gate | `LaunchGate` | Server code (A-I1), claims from `Known` facts (A-I18), links, WCAG 2.2 AA rules and theme contrast, performance budget, legal pages, machine views, quarantine, required languages, red lines, structured data; fast and full tiers |
| Releases | `ReleaseBuilder` | Hashes a build into a `ReleaseManifest`, snapshot or delta; refuses executables, server configuration files and server-side include directives, sniffed by content |
| Site log | `SiteLog`, `ReleaseSigner` | `{seq, manifest}` entries in JCS at leaf `seq − 1`, C2SP tiles, a checkpoint signed by a delegated release key with origin `{domain}/.well-known/awp/log` |
| Hosts | `SiteHost`, `DirectoryHost` | Files, manifest, tiles, checkpoint, then the pointer; a prefix per release with every file version stored once (`Atomicity::PrefixPointer`); exact rollback; a conformance suite every host runs |
| Publishing | `PublishingPipeline` | Draft, check, preview, approve, release, publish, announce, measure; corrections, withdrawals (notice or 410), schedules, per-item revert, whole-site rollback |
| Media | `MediaPipeline` | Type sniffing, executables and polyglots refused, location metadata stripped by re-encoding, resizing, alt text that a person confirms |
| Lanes | `lanes`, `LiveStore` | Release, live and stream lanes per field, declared per kind in `site.toml`; claims stay in releases (A-I26) |
| Streams | `StreamLane`, `EntrySigner` | Live-blog entries signed by the edge key in the AWP 2.0 proof format, served at once, folded into signed releases every five minutes and at the close |
| Constant updates | `UpdateEngine`, `Batcher`, `FeedMapping` | Model-free feed mappings, feed bounds that hold a delivery whole (A-I27), batch windows per kind, exceptions held alone, fast checks per item, one release and one `catalog.changed` per batch, scheduled full checks, per-item revert and replay |

## Content repository

| Path | Holds |
|------|-------|
| `site.toml` | Name, domain, languages (the first is the default), jurisdictions, `security.txt` contacts, budgets, redirects, capabilities per kind |
| `content/<kind>/<slug>/<language>.md` | One item per language: YAML (`---`) or TOML (`+++`) front matter, then the Markdown body |
| `content/<kind>/<slug>/*` | The item's media |
| `public-terms.json` | The charter's public terms; their `usage` goes into `robots.txt` |
| `capabilities.json` | The site's capabilities, listed in machine views |
| `theme/` | `theme.toml` tokens, `templates/*.html`, `style.css` |

Kind directories are `pages`, `posts`, `articles`, `products`, `events`, `notices`,
`services`, `people` and `collections`.

```markdown
---
title: Panel 400 W
summary: A 400 W monocrystalline panel.
price: 25000
currency: KES
stock: 12
volatile: [stock]
state: published
---
Rated at 400 W.
```

## Publishing a release

```rust
use std::sync::Arc;
use adk_core::audit_log::InMemoryAuditLogStore;
use adk_site::content::SiteProject;
use adk_site::generator::{BuiltinGenerator, SiteGenerator};
use adk_site::host::{DirectoryHost, HostTarget, SiteHost};
use adk_site::log::SiteLog;
use adk_site::release::{ReleaseBuilder, ReleaseOptions};
use adk_site::signer::Ed25519ReleaseSigner;
use awp_types::ReleaseSource;

async fn publish(repo: &std::path::Path, out: &std::path::Path, served: &std::path::Path) -> adk_site::Result<()> {
    let project = SiteProject::load(repo, chrono::Utc::now()).await?;
    let report = BuiltinGenerator.build(&project, out).await?;

    let domain = project.config.domain.clone();
    let signer = Arc::new(Ed25519ReleaseSigner::generate(&domain)?);
    let log = SiteLog::open(Arc::new(InMemoryAuditLogStore::new()), &domain, signer).await?;
    let options = ReleaseOptions {
        source: ReleaseSource::Onboarding,
        generator: report.generator.clone(),
        charter_version: project.config.charter_version,
        redirects: report.redirects.clone(),
        written: None,
        force_snapshot: false,
    };
    let release = ReleaseBuilder::new().release(out, None, options, &log).await?;
    DirectoryHost::new(served).publish(&release, HostTarget::Production).await?;
    Ok(())
}
```

`SiteLog` stores its tree in any `adk_core::audit_log::AuditLogStore`; the SQLite and
PostgreSQL stores in `adk-session` keep it across restarts.

## Constantly updated sites

A shop syncing its supplier every minute or a newsroom on election night stays static and
signed: `UpdateEngine` sorts each update into its lane, batches releases per kind (30 s by
default, 10 s for articles, 60 s for products), holds a feed delivery that breaks its bounds
whole with a P2 alarm, holds out-of-rule changes alone and publishes the rest without a model.
Live blogs run as streams whose signed entries fold into releases every five minutes.
`DirectoryHost::new(root).materialise(false).retain_releases(n)` publishes at the cost of
what changed. See [Constantly updated sites](../docs/official_docs/deployment/constant-updates.md).

```toml
[lanes.product]
volatile = ["stock"]
```

## Verifying from published files

A verifier reads only what the host serves: the checkpoint, the tiles a proof needs, and the
manifest.

```rust
use adk_auth::signed_audit::NoteVerifier;
use adk_site::host::{HostTiles, SiteHost};
use adk_site::log::{CHECKPOINT_PATH, TileProofs, verify_checkpoint, verify_release_inclusion};
use adk_site::release::{file_digest, manifest_path};

async fn verify(host: &dyn SiteHost, domain: &str, key: &NoteVerifier) -> adk_site::Result<()> {
    let note = host.fetch(CHECKPOINT_PATH).await?.unwrap_or_default();
    let checkpoint = verify_checkpoint(&String::from_utf8_lossy(&note), domain, std::slice::from_ref(key))?;
    let seq = checkpoint.size;
    let manifest = host.fetch(&manifest_path(seq)).await?.unwrap_or_default();
    let tiles = HostTiles(host);
    let proofs = TileProofs::new(&tiles, seq);
    let entry = proofs.entry(seq - 1).await?;
    let proof = proofs.inclusion_proof(seq - 1).await?;
    verify_release_inclusion(&checkpoint, &entry, &proof, seq, &file_digest(&manifest))
}
```

## Migrating a site (feature `migrate`)

```toml
[dependencies]
adk-site = { version = "3.0.0", features = ["migrate"] }
```

| Stage | Type | Behaviour |
|-------|------|-----------|
| Domain claim | `DomainClaim`, `ProvenDomain` | DNS TXT or well-known token; until proven only polite public reads run (A-I9) |
| Sources | `WordPressRest`, `Wxr`, `Crawl`, `Presence`, `IsolatedReplay` | A `SiteInventory`; requests signed with the site's Web Bot Auth key, robots-respecting, rate-limited, recorded |
| Detectors and quarantine | `Detectors`, `QuarantineBook` | Hidden-link spam, cloaking, executables and MIME mismatches, obfuscated scripts; released per item through an `ApprovalGate`, executables never |
| Sanitiser | `Sanitiser` | HTML to Markdown through an allowlist |
| Feature mapping | `map_features` | `submit_inquiry`, `search_site`, `get_quote`, `place_order`, `check_availability`, `book`; owner decisions for comments and accounts |
| Freeze | `Freeze` | A private-vantage crawl published as a signed release; no upstream request at serve time |
| Parity | `MigrationParity` | URL coverage, perceptual difference, main-text similarity, metadata |
| Probes | `Prober` | Drift on the host and at the origin against the release window; certificate, DNS, headers |
| Archive | `LegacyArchive` | The legacy export encrypted through `adk-artifact`; never deleted (A-I10) |

See [Site migration](../docs/official_docs/deployment/site-migration.md).

## Writing a host

Implement `SiteHost` and run `adk_site::host::conformance::run_all` in its tests: it checks
the publication order, atomicity under concurrent readers for snapshots and deltas, exact
rollback, the redirect round trip and delta publication. Declare honest `limits()`:
`batch::check_host` refuses a host without atomic publication for batch windows under a minute.
