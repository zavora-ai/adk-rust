# Spin up an AWP website

An AWP website is static content in signed releases, an agent edge beside it, and a kernel that
runs the site's team of agents (spec 3.5 §7.17, §7.22). This page starts one from nothing.

> **Note:** `adk-website` and `adk-site` are **Experimental**. Their commands and the project
> layout may change while spec 3.5 lands.

| Start with | Command | What you get |
|------------|---------|--------------|
| The command line | `adk-rust website new <dir>` | The site project, its kernel and edge running on this machine, the owner's views on a loopback web home, a setup code, and onboarding open in the browser |
| The product's binary | `adk-website new <dir>`, then `adk-website start <dir>` | The same project and the same kernel and edge, without the web home: for a server or a container |
| A Rust project | `cargo adk new my-site --template website` | The project for the business kind, plus an edge binary you own and extend |

## 1. Start the site

```bash
cargo install adk-cli --features website   # until 3.0.0 is on crates.io: cargo install --path adk-cli --features website
adk-rust website new juniper --kind business --name "Juniper Bakery" --domain juniper.example --theme meadow
```

```text
Scaffolded a business site in juniper:
  site.toml
  …
juniper-example (juniper.example) is running:
  edge    http://127.0.0.1:8080  the site and its agent edge
  kernel  http://127.0.0.1:7340  the control plane
  home    http://127.0.0.1:8090/ui/website  onboarding, the charter, the site and the review
Setup code: …  — enter it on the setup screen to pair your device; nothing else can claim the site (A-I20).
Your keys — run these yourself, in your own terminal, never inside an agent:
  1. adk-rust awp keys rotate --site juniper --new-root-key ~/keys/juniper.example-root.jwk
  …
```

The kinds are `personal`, `business`, `ecommerce`, `government`, `nonprofit`, `education`,
`media`, `health`, `community` and `events`, each with its own agents, capabilities and rules
([Site kinds](adk-website.md#site-kinds)); the themes are `paper`, `harbour` and `meadow`. A
directory that already holds a `site.toml` is started as it is.
[Spinning up a website](site-cli.md#spinning-up-a-website) lists every flag, `--agency`
among them.

The project:

| Path | Holds |
|------|-------|
| `site.toml` | Name, domain, languages and the kind's site rules |
| `theme/` | `theme.toml` with the theme's design tokens, `style.css` |
| `charter.draft.json` | Charter version 1 drafted from the kind's template, unsigned ([The charter](charter.md)) |
| `capabilities.json`, `public-terms.json` | The kind's capabilities and the charter's public terms |
| `content/` | The content repository |
| `data/` | The site log, the host, the delegated keys, and the kernel's and the edge's stores (`kernel.db`, `edge.db`), so charters, KPIs, approvals, leads and held calls survive a restart |
| `public/index.html` | The holding page served until the launch |

The project keeps no root key.

## 2. Pair your device

The site takes no onboarding input until the setup code pairs the owner's device (A-I20). Open
the home, enter the code on the setup screen, and the device becomes the owner's. A restarted
site whose owner is paired offers no second code.

## 3. Give the site its keys

A site is verifiable once its root key signs the contract and delegates the keys the site signs
with. Run the three commands the start printed, at your own terminal; root operations refuse to
run without one, and the root key file stays off the server (spec 3.5 D-11, A-I5):

```bash
adk-rust awp keys rotate --site juniper --new-root-key ~/keys/juniper.example-root.jwk
adk-rust awp keys delegate --site juniper --root-key ~/keys/juniper.example-root.jwk --scope release --key juniper/data/keys/release.jwk
adk-rust awp keys delegate --site juniper --root-key ~/keys/juniper.example-root.jwk --scope card,receipt --key juniper/data/keys/card.jwk
```

`rotate` prints the `_awp-root.juniper.example` TXT record to publish, and writes the signed
contract and `keys.json` to `well-known/`. Until the release key is delegated the site publishes
nothing: the launch gate's `release-delegation` finding names these commands.

## 4. Onboard

Onboarding is nine stages the Steward leads: start, discover, protect, interview, charter,
build, launch gate, connect and deploy, run ([Onboarding](adk-website.md#onboarding)). The owner
works through them on the home: what discovery found with its sources, the interview with a
live preview, the charter section by section, the looks, the launch check with every page, the
connections and going live ([The Website's Views](../surfaces/website.md)).

| Rule | Invariant |
|------|-----------|
| Discovery records what it finds as `Assumed`, citing its source; only the owner's own words make a fact `Known`, and only `Known` facts appear as claims | A-I18 |
| No KPI has a target until it has a baseline, and the owner sets targets | A-I14 |
| The site never asks for a password: connections use OAuth, the credential broker or pairing | A-I19 |
| The charter takes effect when every required signer signs | A-I23 |
| Production publication at the launch is held for the owner | A-I21 |
| Onboarding acts only with the owner's approval of each change; every other job starts at Observe or Propose, and only the owner raises a job's autonomy | A-I13 |

`adk-website plan` prints the stages for a starting point — `nothing`, `site <url>`, or
`presence <url>…` — and the scripted example shows the views over a scripted pipeline:

```bash
cargo run -p adk-surface --features website --example scripted_website -- 8803 /tmp/website
```

## 5. Look after it

| Command | Shows or does |
|---------|---------------|
| `adk-rust website status` | The kind, the charter, whether the site is live and its release, paired devices, the jobs |
| `adk-rust website review` | The weekly review: KPIs against baselines, work, money, experiments, incidents, alarms, promotions and questions |
| `adk-rust website charter` | The charter in force with its signing decision and every earlier version, or the draft |
| `adk-rust website jobs promote JOB` | Raises a job one level by the owner's decision and grants the standing rules it needs (A-I13). A level above the charter's falls back to the charter's the next time the site starts ([Remaining work](../../design/spec-3.5-agentic-web.md#remaining-work)) |

Each takes `--site DIR` ([`adk-rust website`](site-cli.md#spinning-up-a-website)). Content,
releases and rollbacks are `adk-rust site` ([Sites and AWP from the command line](site-cli.md)).

## 6. Choose where it runs

`adk-website deploy` writes a deployment shape into the project and prints its commands and
the monthly running cost in minor units: hosting, the domain's yearly cost spread over twelve
months, and each running agent's model runs at the prices `--price <input>/<output>` gives per
million tokens.

```bash
cargo install adk-website   # until 3.0.0 is on crates.io: cargo install --path adk-website
adk-website deploy juniper --shape container --hosting 2000 --domain-cost 1500 --price 300/1500
```

| Shape | Content | Edge and kernel | Files written |
|-------|---------|-----------------|---------------|
| `local` | `DirectoryHost`, or any host adapter | `adk-rust up --install`, then `adk-website start` | None |
| `container` | Any static host | One process serving the edge publicly and the kernel on loopback | `Dockerfile` (installs `adk-website` from crates.io; no image has been built from it yet), `.dockerignore`, `scripts/build-image.sh` |
| `managed` | Any static host | The managed agent runtime through `adk-deploy` | `adk-deploy.toml` |
| `agency` | Per-site hosts | `adk-website agency agency.toml`: many sites in one harness, each isolated (A-I22) | `agency.toml` |

`adk-website start <dir> [--edge ADDR] [--kernel ADDR] [--owner NAME]` serves a project on a
server: the edge on `127.0.0.1:8080` and the kernel on `127.0.0.1:7340` by default, the setup
code in its logs, and no web home; the views mount on any server with
`adk_surface::web::website::router`
([Mount it on your own server](../surfaces/website.md#mount-it-on-your-own-server)). Content
goes to any of `adk-site`'s host adapters ([Site hosts](site-hosts.md)).

## The developer route

The template's project builds and serves itself with `cargo` and `adk-rust site`:

```bash
cargo adk new my-site --template website
cd my-site
cargo run -- check                          # build the working copy and run the launch gate
adk-rust awp keys rotate --new-root-key ~/keys/my-site-root.jwk
adk-rust awp keys delegate --root-key ~/keys/my-site-root.jwk --scope release
adk-rust site publish pages/about/en pages/privacy/en pages/consent/en pages/terms/en --yes
cargo run                                    # the release and the edge at http://127.0.0.1:8080
```

`cargo run -- check` exits 1 while the launch gate fails; the About and legal pages are drafts
until you publish them. `delegate` without `--key` writes a new `data/keys/release.jwk` and
never overwrites one. `cargo run` serves the release `adk-rust site publish` put in
`data/host`, or the holding page before the first one, and the capabilities declared in
`capabilities.json` that `src/main.rs` registers a handler for — `search_site` in the template.
It signs its Agent Card and receipts with `data/keys/card.jwk` (and `receipt.jwk` when there
is one), which it makes on its first start and reads after: before the edge faces the public,
delegate that key with `adk-rust awp keys delegate --root-key ~/keys/my-site-root.jwk --scope
card,receipt --key data/keys/card.jwk`. To add
capabilities, see [Writing capability packs](capability-packs.md).

## Check the result

The scenario example runs spec 3.5's scenario S1 — from nothing to a live, signed site — on a
scripted model, and asserts each step:

```bash
cargo run --manifest-path examples/website/Cargo.toml -- S1
```

```text
  S1 From nothing to live
    ok   A-I20: no input before the setup code pairs one device: unpaired and guessed tokens refused; the code paired one device only
    ...
    ok   charter v1 signed; every KPI carries a baseline source and no target (A-I14): version 1, 6 KPIs
    ok   launch gate green: passed
    ok   A-I21: the launch is the owner's decision: held ["site_publish"], published true
    ok   awp verify passes on the local deployment: failure None, release Some(1), 20 pages sampled
| S1 | From nothing to live | scripted | PASS (20/20 checks) |
```

`--live` runs the Steward's interview and the launch on `claude-sonnet-5-5`, or on
`gemini-3.7-flash` with `--provider gemini` ([`examples/website`](../../../examples/website/README.md)).
`adk-rust awp verify <domain>` checks a published site from anywhere
([Sites and AWP from the command line](site-cli.md#adk-rust-awp)).

## Related

- [The AWP website product](adk-website.md)
- [Move a WordPress site to AWP](wordpress-to-awp.md)
- [The charter](charter.md)
- [Agentic Web Protocol (AWP) 2.0](awp.md)
