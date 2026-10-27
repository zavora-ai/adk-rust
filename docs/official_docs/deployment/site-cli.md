# Sites and AWP from the command line

`adk-rust site …` runs an AWP 2.0 site's pipeline on its content repository — drafts,
schedules, corrections, withdrawals, per-item reverts, builds, signed releases, rollbacks,
migration and freeze — and `adk-rust awp …` verifies any domain, runs the conformance kit,
manages the root and delegated keys and migrates an AWP 1.x `business.toml` (spec 3.5 §8.1).
Both are in `adk-cli`'s `website` feature.

> **Note:** the site pipeline (`adk-site`) is **Experimental**. Its commands and the project
> layout may change while spec 3.5 lands.

```bash
cargo install adk-cli --features website
```

## Spinning up a website

`adk-rust website new [dir]` scaffolds a site project (the files `adk-website new` writes),
starts its kernel and agent edge on this machine, mounts the website views of the web home
beside them, prints a one-time setup code and the owner's key steps, and opens onboarding in
the browser (spec 3.5 §7.17). A directory that already holds a `site.toml` is started as it
is.

```text
$ adk-rust website new bakery --kind business --name "Juniper Bakery" --domain bakery.example
Scaffolded a business site in bakery:
  site.toml
  …
bakery-example (bakery.example) is running:
  edge    http://127.0.0.1:8080  the site and its agent edge
  kernel  http://127.0.0.1:7340  the control plane
  home    http://127.0.0.1:8090/ui/website  onboarding, the charter, the site and the review
Setup code: ABCD-EFGH-2345  — enter it on the setup screen to pair your device; nothing else can claim the site (A-I20).
Your keys — run these yourself, in your own terminal, never inside an agent:
  1. adk-rust awp keys rotate --site bakery --new-root-key ~/keys/bakery.example-root.jwk
  …
```

| Flag | Default | Use |
|---|---|---|
| `--kind`, `--name`, `--domain`, `--language`, `--theme`, `--currency` | business, the directory's name, `example.com`, `en`, `paper`, `USD` | The scaffold |
| `--agency` | — | One harness for many sites: the site goes into `<dir>/<domain>`, `<dir>/agency.toml` lists it, and every listed site is served, each isolated (A-I22) |
| `--edge`, `--kernel`, `--home` | `127.0.0.1:8080`, `127.0.0.1:7340`, `127.0.0.1:8090` | Listeners; the home takes loopback only |
| `--owner` | your user name | The person the setup code pairs |
| `--no-open` | — | Do not open the browser |

The project keeps no root key: the root key file stays with the owner, and `awp keys` signs
with it at a terminal. Until a device is paired the site accepts no onboarding input.

| Command | Shows or does |
|---|---|
| `website status [--json]` | The kind, the charter in force or the draft, whether the site is live and its release, paired devices, the jobs |
| `website review [--week DATE] [--json]` | The weekly review, composed from the site's stores: KPIs against baselines, work, money, experiments, incidents, alarms, promotions offered, demotions and questions |
| `website charter [--json]` | The charter in force with its signing decision, every earlier version, or the draft waiting for its signers |
| `website jobs [--json]` | Every job and its autonomy level |
| `website jobs promote JOB [--yes]` | Raises the job one level by the owner's decision and grants the standing rules the level needs; a person the charter does not name as an owner is refused (A-I13) |
| `website jobs demote JOB [--reason TEXT] [--yes]` | Lowers the job one level and revokes the standing rules it no longer warrants |

Every command takes `--site DIR` (default `.`) and `--owner NAME` (default: the charter's
first owner). The standing rules are kept in the site's `data/kernel.db`.

## The site project

Every `site` command acts on a project directory: the current one, or `--site DIR`.
`adk-website new` scaffolds one, `cargo adk new my-site --template website` scaffolds the same
project with an edge binary that serves it, and a `site.toml` is enough to start.

```toml
name = "Example Solar"
domain = "example.com"
languages = ["en"]
```

| Path | Holds |
|---|---|
| `site.toml`, `content/`, `theme/`, `public-terms.json`, `capabilities.json` | The content repository ([`adk-site`](../../../adk-site/README.md#content-repository)) |
| `well-known/` | Files released under `/.well-known/`: the root-signed contract and `keys.json` that `awp keys` writes |
| `static/` | Files released at their own paths, such as a migrated site's uploads |
| `feeds.json` | The feeds the owner configured: `[{"mapping": …, "bounds": …}]` |
| `packs/` | Installed capability packs: `trust.json`, each `{name}.pack.json` as signed, and their driver manifests in `drivers/{name}/` ([Signed bundles](capability-packs.md#signed-bundles)) |
| `data/site-log.db` | The site log (SQLite) |
| `data/history/` | Every saved version of every item, for reverts and replays |
| `data/keys/` | `release.jwk` signs the log; `web-bot-auth.jwk` signs crawls; `archive.key` seals legacy exports. Each is created owner-only on first use |
| `data/host/` | The `DirectoryHost` releases publish to, unless `--host-dir DIR` names another |
| `data/updates/` | The update engine's journal that `site batches` and `site feeds` read |
| `data/audit.jsonl` | The audit trail: the pipeline's `site.content.*` and `site.release.*` records, `awp keys`' `awp.keys.*`, and a served site's records |
| `data/claims/`, `data/migration/`, `data/archive/` | Domain claims, migration reports, the encrypted legacy archive |

Every change is the act of the person at the terminal: the command shows the page diff and
asks, and `--yes` answers for a script. A site whose charter (`charter.json`, or the draft
`charter.draft.json`) names an approval chain of two or more roles for `publish` is refused:
it publishes only through its approvals, each role by a different person (A-I23).

## `adk-rust site`

| Command | Does |
|---|---|
| `site build [--preview] [--json] [--legacy URL]` | Builds into `data/work/build` and runs the launch gate's full tier; exits 1 when it fails. After `site migrate`, the tier includes migration parity over the inventory and design choice the migration recorded (`data/migration/inventory.json`, `report.json`); `--legacy` compares the looks with the legacy site served at `URL` in headless Chrome |
| `site publish [ITEM…] [--yes]` | Releases the working copy, or publishes the named drafts; scheduled items whose time has come publish first. The first release passes the full tier — migration parity included after a migration — later ones the fast tier on the changed items |
| `site rollback SEQ [--replay all\|none] [--yes]` | Serves release `SEQ`'s files again as a new release, puts the working copy back, lists the changes since and replays each one you choose |
| `site log [--json]` | Every release: time, snapshot or delta, files, source and manifest digest; the checkpoint |
| `site content new KIND --title T [--slug S] [--language L]` | A draft item in `content/` |
| `site schedule ITEM TIME [--embargo-until T] [--expires-at T] [--revert-at T]` | Schedules a draft (`TIME` in RFC 3339) |
| `site correct ITEM --note TEXT [--set FIELD=VALUE…] [--body FILE]` | A correction with a visible note; both versions stay in the log |
| `site withdraw ITEM --gone \| --notice TEXT` | The page answers 410 Gone or shows a notice |
| `site revert ITEM [--to SEQ]` | Publishes the item's earlier version and keeps every other change |
| `site batches [--json]` | The batches the update engine closed: release, items, superseded versions, held items and why, approval-to-live |
| `site feeds [--json]` | The configured feeds with their bounds and rates; model calls are always zero |
| `site migrate URL \| --wxr FILE [--uploads DIR] \| --backup DIR --origin URL` | Moves an existing site into the project ([below](#migration-and-freeze)) |
| `site freeze URL --vantage URL [--edge URL]` | Publishes a crawled snapshot of the legacy site as a signed release |
| `site packs list [--json]`, `install FILE`, `remove NAME`, `trust PUBLISHER KEY`, `sign FILE --key KEY` | Capability packs as signed bundles: install only from publishers in `packs/trust.json`, refused when unsigned, tampered, invalid, conflicting or a downgrade ([Trusting and installing](capability-packs.md#trusting-and-installing)) |

Item ids are `<kind directory>/<slug>/<language>`, such as `posts/hello-world/en`.

```text
$ adk-rust site content new post --title "Hello world"
Drafted posts/hello-world/en at ./content/posts/hello-world/en.md.
$ adk-rust site publish posts/hello-world/en --yes
…
Published release 2 to directory (26 files uploaded).
$ adk-rust site revert posts/hello-world/en --yes
Published release 4: posts/hello-world/en
$ adk-rust site rollback 1 --replay none --yes
Rolled back: release 6 serves the files of release 1.
```

`site batches` reads the engine's journal, `data/updates/batches.jsonl`, where the CLI appends
every batch its engine closes (reverts and replays). `site feeds` validates the feeds in
`feeds.json` through the engine and lists them with their bounds, with delivery counters from
`data/updates/feeds.json` when that file is present.

## Making a site anyone can verify

A site is verifiable once its root key signs the contract and a delegation of its release key,
and a release carries both. Root operations run only at a terminal and ask before they sign:
an agent's shell has no terminal, so no agent runs them (A-I5).

```bash
# The site's first root; keep the file off the host and out of every agent's reach.
adk-rust awp keys rotate --new-root-key ~/keys/example-root.jwk
# Delegate the release key `site publish` signs the log with, for 90 days.
adk-rust awp keys delegate --root-key ~/keys/example-root.jwk --scope release --key data/keys/release.jwk
adk-rust site publish --yes
```

`awp keys rotate` prints the `_awp-root.<domain>` TXT record to publish. With `--root-key` it
rotates: the new root is endorsed by the old one, so verifiers that pinned the old root accept
it. With `--recover` (the old root is lost) the new root is anchored by DNS alone and every
delegation is made again. Both commands write `well-known/awp/keys.json` and sign
`well-known/awp.json` again; a key file is never overwritten and is created owner-only. Each
appends `awp.keys.rotated` or `awp.keys.delegated` to `data/audit.jsonl`, naming the root key
that signed it.

## `adk-rust awp`

| Command | Does |
|---|---|
| `awp verify DOMAIN` | The verification procedure with `GET` only and no redirects: contract and keys against the DNS root, delegations, the A2A card, the checkpoint extending the pinned one, the current release's inclusion, 20 sampled files; then posture and readiness. Exits 1 naming the broken link |
| `awp conformance URL` | The AWP 2.0 conformance kit; `--active` runs the edge's `GET` checks, only against a deployment on loopback (A-I9) |
| `awp keys rotate`, `awp keys delegate` | Root and delegated keys ([above](#making-a-site-anyone-can-verify)) |
| `awp migrate-config business.toml [--out DIR]` | `site.toml`, `capabilities.json`, `public-terms.json`, `charter.draft.json` and an unsigned `contract.draft.json`, with every inference listed for onboarding to confirm; never overwrites |

| `awp verify` flag | Use |
|---|---|
| `--origin URL` | Read the domain's documents from a local or staging deployment |
| `--anchor RECORD` | Check the root against this `_awp-root` record instead of DNS, before DNS is live |
| `--samples N`, `--at TIME` | Files sampled; check live documents as of a time |
| `--pins FILE` | Where each domain's root and last checkpoint are pinned (default `<config dir>/adk-rust/awp-pins.json`) |
| `--sign-key FILE --signature-agent ORIGIN` | Sign each request with Web Bot Auth |
| `--json` | The report as JSON |

```text
$ adk-rust awp verify example.com --origin http://127.0.0.1:8080 --anchor "v=awp2; kid=site-root-2026; jkt=gkihQ5…"
awp verify example.com: PASS
  read from http://127.0.0.1:8080
  root anchored by the --anchor record (not DNS)
  release 4, 20 files sampled
posture
  profile       static
  …
```

| Failure | The broken link |
|---|---|
| `contract.signature` | The contract is not signed by the root |
| `root.unanchored` | No `_awp-root` record names the key set's root |
| `card.signature` | The A2A card is not signed by a delegated card key |
| `log.consistency` | The checkpoint does not extend the one this verifier pinned |
| `page.digest` | A served file differs from its release digest |

## Migration and freeze

`site migrate` reads the legacy site — its WordPress REST API, else a crawl; a WordPress
export; or a backup replayed in isolated containers — through adk-site's signed,
robots-respecting client, then detects, converts, quarantines and maps features
([Site migration](site-migration.md)). Items go to `content/`, uploads to `static/`, redirects
to `site.toml`, capability drafts to `capabilities.json`, and the report with the quarantine and
the owner decisions to `data/migration/report.json`. An export is archived encrypted under
`data/archive/` and never deleted (A-I10). Items the project holds already stay unless
`--replace`.

A domain is proven by the claim in `data/claims/<domain>.json`: the first run prints the
`_awp-claim` TXT record and the well-known file that prove it. Until it verifies, only polite
public reads run; `site freeze` refuses (A-I9).

```text
$ adk-rust site migrate --wxr export.xml
Archived the legacy export export.xml encrypted (key a77f1f656351ea47); it is never deleted (A-I10).
Migrated 7 items (7 written, 0 kept as they were), 0 files, 2 new redirects, 3 new capability drafts.
Quarantined, held out until you decide each one:
  media:/wp-content/uploads/2026/10/cache.php — executable
```

## Related

- [Site migration](site-migration.md)
- [Constantly updated sites](constant-updates.md)
- [Agentic Web Protocol](awp.md)
- [Kernel commands](../kernel/cli.md#webhooks): `adk-rust up` serves signed webhooks that wake the kernel's sleepers
