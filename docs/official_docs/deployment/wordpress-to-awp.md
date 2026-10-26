# Move a WordPress site to AWP

A WordPress site runs PHP on every page request. Moving it to AWP 2.0 replaces that with static
files in signed releases: posts, pages and uploads keep their URLs, forms and the shop become
capabilities on the agent edge, and anything planted in the old site is held out with its
evidence (spec 3.5 §7.8, §7.18, S3, S4, S17). This page walks through the move with the
`adk-rust site` commands; [Site migration](site-migration.md) is the library reference behind
them.

> **Note:** `adk-site` is **Experimental**. The commands and the project layout may change
> while spec 3.5 lands.

```bash
cargo install adk-cli --features website
```

Until 3.0.0 is published to crates.io, install from a checkout of the repository:
`cargo install --path adk-cli --features website` and `cargo install --path adk-website`.

| Step | Command | Result |
|------|---------|--------|
| 1. Create the project | `adk-website new`, or a `site.toml` | The project the old site moves into |
| 2. Prove the domain | `adk-rust site migrate` prints the record | Private reads and the freeze unlocked (A-I9) |
| 3. Freeze, when the old server is exposed | `adk-rust site freeze` | A crawled snapshot served as a signed release, in hours |
| 4. Migrate | `adk-rust site migrate` | Content, uploads, redirects and capability drafts; findings quarantined |
| 5. Decide | The migration report | Quarantined items, comments, member areas, the design |
| 6. Build, sign, publish | `adk-rust site build`, `awp keys`, `site publish` | Release 1, signed |
| 7. Switch and verify | DNS, `adk-rust awp verify` | The site served from the static host, checked from outside |

## 1. Create the project

Scaffold with the site's kind, or write a three-line `site.toml`. `adk-website new` writes the
project without starting it; `adk-rust website new` writes it and starts serving it
([Spin up an AWP website](website-quickstart.md)).

```bash
cargo install adk-website
adk-website new blog --kind personal --name "City Walks" --domain citywalks.example
```

```toml
name = "City Walks"
domain = "citywalks.example"
languages = ["en"]
```

Onboarding proposes the kind from what it finds — a blog's posts, a WooCommerce shop, a
`.gov` domain, a donation plugin — and the owner confirms it ([Site kinds](adk-website.md#site-kinds)).
Every `site` command acts on the current directory, or on `--site DIR`.

## 2. Prove the domain

The first `site migrate` against a URL records a claim in `data/claims/<domain>.json` and prints
the two ways to prove control: a TXT record at `_awp-claim.<domain>`, or a token served at
`https://<domain>/.well-known/awp/claim/<token>`. Until the proof verifies, only polite public
reads run: the public REST collections and a crawl, signed with the site's Web Bot Auth key,
`robots.txt` read first, one request at a time. Drafts, the isolated replay and the freeze need
the proof (A-I9). A WordPress export needs no proof.

## 3. Freeze, when the old server is exposed

When the old site runs a vulnerable PHP or plugin, take it off the public path first:

```bash
adk-rust site freeze https://citywalks.example --vantage http://10.0.0.12 --edge https://agent.citywalks.example
```

The freeze crawls the old server from a private vantage point (the public origin mapped onto
`--vantage`), leaves out server code and quarantined files, rewrites each form to post to
`<edge>/awp/v2/capabilities/<name>`, and publishes the snapshot as a signed release. Serving it
reads only the host's files: no request reaches the old server at request time, and the old
server need only answer the crawler. Point the domain at the static host and the edge, then
migrate.

## 4. Migrate

| Source | Command | Reads |
|--------|---------|-------|
| The REST API, else a crawl | `adk-rust site migrate https://citywalks.example` | The public posts, pages, media, categories and tags from `wp-json/wp/v2`, and WooCommerce products from the Store API. Without a REST API, a same-origin crawl from the sitemaps and links. Drafts and menus need a read-scoped application password from the credential broker, which the library's `WordPressRest` takes and the command does not |
| A WordPress export | `adk-rust site migrate --wxr export.xml --uploads wp-content/uploads` | The WXR file; with `--uploads`, every file in the uploads directory, including files no attachment names |
| A backup | `adk-rust site migrate --backup backup/ --origin https://citywalks.example` | A directory with the SQL dump and `uploads/`, replayed in containers on an internal network, crawled from loopback, then destroyed; needs Docker and a proven domain |

`--currency` sets the currency of a shop's prices in an export; `--replace` replaces items and
files the project already holds. The command writes:

| Output | Where |
|--------|-------|
| Items, each keeping its URL through its `path` field | `content/` |
| Uploads, at their paths | `static/` |
| Archives, feeds and legacy URLs as 301 redirects | `site.toml` |
| Capability drafts for the site's forms and features | `capabilities.json` |
| The quarantine, the owner decisions and everything left out | `data/migration/report.json` |
| The export, encrypted; never deleted (A-I10) | `data/archive/` |

```text
$ adk-rust site migrate --wxr export.xml
Archived the legacy export export.xml encrypted (key a77f1f656351ea47); it is never deleted (A-I10).
Migrated 7 items (7 written, 0 kept as they were), 0 files, 2 new redirects, 3 new capability drafts.
Quarantined, held out until you decide each one:
  media:/wp-content/uploads/2026/10/cache.php — executable
```

### What becomes what

| WordPress | AWP site |
|-----------|----------|
| Posts and pages | Content items at the same URLs |
| Uploads | Static files at the same paths; executables and files whose bytes do not match their type are quarantined |
| Category, tag and date archives, feeds | 301 redirects |
| Contact form | `submit_inquiry`: `non_idempotent`, consent scope `contact`, the form's fields as its input schema |
| Search | A static index and `search_site` |
| Comments | An owner decision, off until decided; held comments become `submit_comment` with every call held |
| WooCommerce | Product items, `get_quote` and `place_order` (`known` tier) |
| Bookings | `check_availability` and `book` |
| Newsletter plugin | `subscribe_newsletter` under consent scope `newsletter` |
| Logins and member areas | The owner decision `migration.accounts`: remove, link to a named service, or leave out |
| `/wp-login.php`, `/xmlrpc.php`, `/wp-includes/…`, theme and plugin files | Not served: they answer 410 Gone (a login page the crawl read keeps a 301 to the home page); the Guardian's probes report them as drift if they reappear |
| Links to pages the crawl could not read, such as `robots.txt`-disallowed ones | Their text, without the link |

The capabilities serve on the agent edge ([Writing capability packs](capability-packs.md)).

## 5. Decide

The detectors scan the inventory before conversion and hold each finding's subject out of the
site, with evidence, until the owner decides it:

| Finding | Held | Released by |
|---------|------|-------------|
| Hidden links, spam links | The item | A decision on that item |
| Cloaking: a page served differently to a crawler | The page's item | A decision on that item |
| Obfuscated script | The item or the script | A decision on that item |
| A file whose bytes do not match its type | The upload | A decision on that upload |
| Server code such as a `.php` upload | The upload | Never (A-I1) |

In `adk-website` each finding is a decision card in onboarding, bound to the digest of its
evidence; a decision by the change's proposer keeps the item held. The CLI leaves held subjects
out and lists them in the report.

The owner also chooses the design: `DesignChoice` in `adk-website` onboarding, `--design
keep|new-theme` on `adk-rust site migrate`. Keeping the design gates the launch on visual
parity: every inventoried URL served or redirected, main text at 0.95 similarity or more,
metadata kept, and a perceptual difference under 0.05 at two widths. A new theme gates URL
coverage, text and metadata, reports the perceptual difference without gating on it, and the
owner approves the new look from the preview: in scenario S3 the largest perceptual difference
between the recorded WordPress pages and the built-in theme's is 0.173. The choice is recorded
in the migration audit (`site.migration.decided`). `adk-rust site build` runs no parity check
([Remaining work](../../design/spec-3.5-agentic-web.md#remaining-work)).

## 6. Build, sign and publish

```bash
adk-rust site build
adk-rust awp keys rotate --new-root-key ~/keys/citywalks-root.jwk
adk-rust awp keys delegate --root-key ~/keys/citywalks-root.jwk --scope release --key data/keys/release.jwk
adk-rust site publish --yes
```

`site build` builds into `data/work/build` and runs the launch gate's full tier: no server code
(A-I1), no claim from an unconfirmed fact (A-I18), no broken internal link, WCAG 2.2 AA rules,
the performance budget, legal pages and machine views; quarantined subjects are already out of
the content `site migrate` wrote. It exits 1 when the gate fails. A migrated page that links to
a page the crawl could not read fails the link check until the page is edited. `site publish` signs release 1 into the site
log and publishes it to the project's `DirectoryHost` in `data/host/`, or the directory
`--host-dir` names. Object storage, Netlify, GitHub Pages and SSH servers take releases through
`adk-site`'s host adapters in code ([Site hosts](site-hosts.md)).

## 7. Switch and verify

Point the domain at the static host, publish the `_awp-root` record `awp keys rotate`
printed, and verify the site from outside:

```bash
adk-rust awp verify citywalks.example
```

The verifier checks the contract against the DNS root, the site log, the release's inclusion
and 20 sampled files against their digests, then prints the posture: no server code, the
identity policy, A2A and MCP, usage terms, `security.txt`, HSTS and CSP. Against a deployment
whose DNS is not live yet, add `--origin` and `--anchor`
([`adk-rust awp`](site-cli.md#adk-rust-awp)). On a site `adk-website` runs, the Guardian's
probes then sample the host and the origin every five minutes. A file that no release explains
— a reappearing `/wp-login.php` among them — opens an incident with one P1 alarm, puts the edge
in read-only mode, and rolls back to the last good release: alone where the owner
pre-authorised it, otherwise as a call the owner approves
([The AWP website product](adk-website.md#incidents-alarms-and-the-weekly-review)).

The old site, its database and its export stay where they are: no step deletes them, and
deletion is the owner's act (A-I10).

## What the scenarios show

`examples/website` runs the move against a recorded WordPress site with an `infected` overlay of
inert markers:

| Scenario | Shows |
|----------|-------|
| S3 Migrate and leave the infection behind | Hidden spam links, an obfuscated script, a `.php` upload, a disguised file and crawler-only scripts quarantined with evidence; the contact form and shop mapped to `submit_inquiry`, `search_site`, `get_quote` and `place_order`; zero broken links; every inventoried URL served or redirected; no server-side executable released |
| S4 Freeze in hours | Nothing frozen before the domain is proven; the snapshot a signed release with no server code; zero upstream requests at request time; the frozen contact form posting to the edge |
| S17 A personal site | A blog proposed as a personal site; posts and feeds at their URLs; the newsletter plugin replaced by `subscribe_newsletter`, which answers `451` without a consent receipt |

```bash
cargo run --manifest-path examples/website/Cargo.toml -- S3 S4 S17
```

## Related

- [Site migration](site-migration.md) — the library: sources, detectors, quarantine, freeze, parity, probes, archive
- [Sites and AWP from the command line](site-cli.md)
- [Spin up an AWP website](website-quickstart.md)
- [Agentic Web Protocol (AWP) 2.0](awp.md)
