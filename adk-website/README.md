# adk-website

A prebuilt AWP 2.0 website for ADK-Rust (spec 3.5 Part C). It onboards its owner, builds
or migrates itself, passes its own launch gate, and then works towards the owner's goals with
a team of eight agents inside the authority, budget and guardrails of its charter.

> **Note:** `adk-website` is **Experimental** (see `STABILITY.md`). Its API may change without
> notice while spec 3.5 lands.

```toml
[dependencies]
adk-website = "3.0.0"
```

The product holds no framework code: the agents, tools, partitions and autonomy ladder come
from `adk-harness` (feature `website`); the charter store, KPI tracker, signals, pairing and
processes from `adk-kernel` (feature `charter`); the pipeline, launch gate, releases, migration
and probes from `adk-site`; the wallet from `adk-payments`. This crate decides what a website
is for each kind of site.

## Modules

| Module | Contents | Spec 3.5 |
|--------|----------|----------|
| `kinds` | One charter template per site kind: goals, KPIs with baseline sources, guardrails, agents, capabilities, rules; kind detection from discovery | §7.16.1 |
| `team` | The eight agents with their principals, partitions, modes, wakes, prompts and tools; the back office as a `TeamSpec` led by the Steward; building them over the harness kits | §7.19 |
| `jobs` | The §7.20 jobs, their ladder bindings, first-week authority, and the kernel processes that run them | §7.20, §7.11 |
| `onboarding` | The nine-stage plan, three starting points, the setup-code gate, discovery, release 1, the live preview, the cost estimate, the scaffold | §7.17, §7.18 |
| `themes` | `paper`, `harbour` and `meadow`, each with light and dark tokens that pass WCAG 2.2 AA | §7.16 |
| `packs` | Capability declarations: contact, quote, booking, order, search, components; packs as signed, installable bundles of declarations, handler configuration, playbooks and driver manifests, verified against the site's trust list (`bundle`, `trust`, `builtin`, `installed`) | §7.16, §7.4, §7.23 |
| `edge` | The site on `adk-server`'s agent edge: the packs' handlers, the adapters to the back office and the pipeline, the read-only step, the public router with the release behind the edge | §7.9, §7.10, §7.16, A-I11 |
| `contract` | The site's keys and signed documents: the contract, the key set with its delegations, the public terms, the `_awp-root` record | §7.2, §7.3, §7.7 |
| `deploy` | The four deployment shapes, `serve` for one site and `serve_agency` for many, agency isolation at the kernel and the edge; with a model, the back office hosted on the kernel under the owner's standing rules; the Guardian's probe over the owner's domain claim; `config::configure` reads `email.json`, `upkeep.json`, `wallet.json` and the claim into `SiteOptions` | §7.22, §7.17, §7.19, §7.20, A-I22 |
| `playbooks` | A skill file for every job, installed into each agent's workspace | §7.16, §7.15 |
| `incident` | Drift, incident status, rollback to the last good release by standing rule or proposal, a person's exit | §7.20, A-I11 |
| `inbox` | The Inbox: the site's email and the chat messages of unpaired accounts, answered in their own thread; a flagged message becomes a typed signal | §7.19, §7.10, X12, A-I24 |
| `alarms` | P1, P2 and P3 routing to the owner's channels; the lead handoff card | §7.21 |
| `leads` | Lead scores and the charter's handoff threshold, consent for nurture from the edge's receipts, the durable contact book, nurture sends outside quiet hours | §7.20, S8 |
| `upkeep` | Upgrade proposals for advisories against the edge's lockfile, certificate renewal proposals | §7.20, S10 |
| `publish_delay` | The charter's delay a page the site drafts on its own waits before it can go live | §7.20, S10 |
| `repository` | Files and git as an entry point: `repository.json`'s signed hooks (GitHub, GitLab or Standard Webhooks) on the edge, a push from an authorised committer read through a `CommitSource` (a local repository or a forge's raw endpoint) and proposed on the owner's decision card; anyone else's push becomes a signal | §7.8.2, A-I24 |
| `channel` | The owner's chat channel: a paired person's photos into the Steward's draft, alt text confirmed from the thread | §7.8.2, S18, A-I24 |
| `quotes` | The reviews discovery found, the owner's approvals to quote them, and the launch gate's check | §7.18, S2 |
| `review` | The weekly review, summarised in plain words on the owner's channel | §7.21 |
| `learning` | The `playbook-learning` job: decided experiments and KPIs short of the owner's targets into playbook revision proposals with a diff, evidence and expected effect, decided by the owner on a card; versioned playbooks written into the agents' workspaces | §7.15, A-I13 |
| `wallet` | The wallet as a KPI source and the site's commerce outcomes | §7.14 |
| `experiments` | Charter KPIs and guardrails as `adk-eval` experiments; variants as their own static files; stable assignment of an anonymous bucket; exposures and goals through the beacon; a daily look; a guardrail breach pauses the job and keeps the control; the decision and evidence in the weekly review | §7.12, A-I15 |
| `measurement` | KPI sources that are not drivers: the beacon, host logs, the commerce, action and spend ledgers, agent search at the edge, AEO | §7.12, §7.11 |
| `workflows` | The probe, the upkeep watch, the weekly review, a push to the content repository and the weekly learning run as kernel runs | §7.20, §7.8.2, §7.15 |
| `observability` | Spans, metrics and the site view | §7.24 |
| `ui` | `WebsiteDesk`: the setup code, onboarding, the charter cards, the site view, the weekly review and the web editor as views in plain words, and the decisions people make on them, saved across restarts with a `DeskStore`; `adk-surface`'s `website` feature serves them on the web home | §7.8.2, §7.16, §7.18, §7.21, §7.24, §8.2 |

## Site kinds

| Kind | Agents | Rules the template adds |
|------|--------|--------------------------|
| Personal | Concierge, Steward, Growth, Guardian | Smallest budget; no advertising or sales; private about the owner |
| Business | All eight | The lead, nurture and handoff loop |
| E-commerce | All eight | Payment policy, product feeds, fraud and refund guardrails |
| Government | Concierge, Agent desk, Inbox, Steward, Growth, Guardian | Editor and publisher approval chain; WCAG 2.2 AA; every official language; no third-party scripts; no advertising or profit KPIs; records retention; AI disclosure on every reply |
| Nonprofit | All eight | Donor consent and receipts; tax-receipt fields |
| Education | The public three, Steward, Growth, Guardian, Revenue | Safeguarding; no profiling of minors |
| Media | Concierge, Steward, Growth, Guardian, Revenue | Content-usage terms; corrections policy; never generated news |
| Health | The public three, Steward, Growth, Guardian | No clinical advice; special-category data; emergency routing |
| Community | Concierge, Steward, Growth, Guardian, Revenue | Low budget; P2 alarms wait for the weekly review |
| Events | All eight | Date-driven freshness; wind-down and archive |

## Onboarding

| # | Stage | What the site does |
|---|-------|--------------------|
| 1 | Start | Asks for a URL, a name or nothing, and a phone number for P1 alarms |
| 2 | Discover | Reads the existing site or presence, records every fact as `Assumed` with its source, proposes the kind |
| 3 | Protect | Offers the freeze for a site on a legacy runtime |
| 4 | Interview | Asks only what discovery could not answer; each answer rebuilds the live preview |
| 5 | Charter | Drafts charter version 1 from the kind's template; every required signer signs |
| 6 | Build | Release 1: every page serves a goal, claims only `Known` facts, every language |
| 7 | Launch gate | The full launch gate over the working copy |
| 8 | Connect and deploy | The connection notice first; the launch is the owner's decision |
| 9 | Run | The 30/60/90-day plan; Propose for the first week; the first review on day 7 |

A fresh site accepts no onboarding input until its setup code pairs the owner's device
(`SetupGate`, A-I20).

```rust
use std::sync::Arc;

use adk_harness::website::SiteRef;
use adk_kernel::pairing::{InMemoryDeviceStore, Pairing};
use adk_website::kinds::{CharterInputs, template};
use adk_website::onboarding::{SetupGate, StartingPoint, plan};
use adk_website::team::SiteTeam;
use awp_types::SiteKind;

async fn start() -> adk_core::Result<()> {
    let gate = SetupGate::new(Pairing::new(Arc::new(InMemoryDeviceStore::new())), "amina");
    let code = gate.issue().await?; // written to the logs
    let device = gate.claim(&code.code, "Amina's phone").await?;
    gate.admit(Some(&device.token)).await?;

    let business = template(SiteKind::Business);
    let steps = plan(&StartingPoint::Nothing, Some(&business));
    assert_eq!(steps.len(), 11);

    let id = uuid::Uuid::new_v4();
    let team = SiteTeam::new(SiteRef::new(id, "juniper", "https://juniper.example"), &business);
    let inputs = CharterInputs::new(id, "Juniper Bakery", "amina", "Fresh bread every morning.", vec!["en".parse().unwrap()], "KES");
    let charter = business.draft_charter(&team, &inputs);
    assert!(charter.validate().is_ok());
    Ok(())
}
```

## The binary

| Command | Does |
|---------|------|
| `adk-website new <dir> [--kind <kind>] [--name <name>] [--domain <domain>] [--language <tag>]... [--theme <theme>] [--currency <code>]` | Scaffolds the site project: `site.toml`, `theme/`, `charter.draft.json`, `capabilities.json`, `public-terms.json`, `content/`, `data/`, `public/index.html` |
| `adk-website kinds` | Lists the kinds, their agents and capabilities |
| `adk-website plan [nothing \| site <url> \| presence <url>...]` | Prints the onboarding plan for a starting point |
| `adk-website start <dir> [--edge <addr>] [--kernel <addr>] [--edge-url <url>] [--owner <name>]` | Serves the site: the kernel's control plane with device pairing, and the agent edge with the release (or the holding page) behind it; prints the setup code |
| `adk-website agency <file> [--edge <addr>] [--kernel <addr>] [--owner <name>]` | Serves every site an agency file lists from one harness, each at its own host |
| `adk-website deploy <dir> --shape local\|container\|managed\|agency [--hosting <minor>] [--domain-cost <minor>] [--price <in>/<out>]` | Writes the shape's files into the project and prints its commands and monthly running cost |

## The agent edge

`edge::SiteEdge::build` declares the kind's capabilities on `adk-server`'s `AwpEdge` with the
packs' handlers; the edge runs the governed path — trust tiers, schemas, idempotency keys,
consent receipts, holds — before a handler sees a call.

| Handler | Capabilities |
|---------|--------------|
| `FormHandler` | `submit_inquiry`, `subscribe_newsletter`, `request_prospectus`, `licensing_enquiry`, `volunteer`, `register`, `submit_service_form`: a lead, a contact, one `lead_captured` signal |
| `QuoteHandler`, `OrderHandler` | `get_quote`, `place_order`, `order_status`, `request_return`: the quote keeps the signed release's price into the order (A-I25); an order is its buyer's |
| `PaymentHandler` | `pay_order`, `donate`, `subscribe`, `buy_ticket`: through the owner's `PaymentDesk`, refused without one; `pay_order` takes the buyer's AP2 mandate or a rail credential, and pays an order once; `awp.payment.created`, `settled` and `rejected` records with an audit trail |
| `BookingHandler` | `check_availability`, `book`, `book_appointment` |
| `SearchHandler`, `ListingHandler`, `StatusHandler` | `search_site`, the listings of published items, `lookup_status` |

Quotes, orders and bookings live in an `edge::commerce::CommerceStore`:
`SiteEdge::build_with_commerce` takes a durable one (`SqliteCommerceStore`, with
`StoredCalendar` for `calendar.json`'s slots), and `SiteEdge::build` keeps them in memory. An
order moves from `awaiting_payment` to `paying` by the store's compare-and-set, so two payments
for one order never both start.

`edge::ap2::Ap2Desk` takes payments under AP2 with the site as the merchant, over
`adk-payments`' `Ap2Adapter` and the owner's payment processor; `edge::token::TokenDesk` takes
`pay_order`'s single-use `credential` on token rails such as `card`, and
`edge::token::PaymentRails` serves both by rail. `edge::caldav::CalDavCalendar` books the
owner's slots in any CalDAV calendar, each slot once. `packs::ApprovalThresholds`
(`approvals.json` in a served site) holds orders, bookings and payments above the owner's
amounts for a decision. `TeamKit::wallet` gives the Finance agent `wallet_pay` and
`wallet_status` over the owner's wallet.

`SiteEdge::status` is the incident path's `StatusSink`: drift puts the edge in read-only mode,
so every non-read capability answers `503 read_only` with the signed status while pages and
read-only capabilities keep working; a person's close resumes it (A-I11).

## Deployment shapes

| Shape | Content | Edge and kernel | Fits |
|-------|---------|-----------------|------|
| `local` | `DirectoryHost`, or any host driver | `adk-rust up --install` on the owner's machine | Trials, developers |
| `container` | Any static host | One container: one process serving the edge and the kernel on two listeners | Most small sites |
| `managed` | Any static host | The managed agent runtime or another agent platform through `adk-deploy` | Owners who want no servers |
| `agency` | Per-site hosts | One harness for many sites, each isolated (A-I22) | Developers running sites for clients |

## A served site

`deploy::serve` runs one site from its project, in the layout `adk-rust site` uses, so the
CLI and the running site share one log, host and journal:

| Path | Holds |
|------|-------|
| `site.toml`, `content/`, `theme/`, `public-terms.json`, `capabilities.json` | The content repository |
| `well-known/`, `static/` | Released under `/.well-known/` (the owner's signed contract and `keys.json`) and at their own paths |
| `feeds.json`, `calendar.json`, `payments.json` | Feeds with their bounds, bookable slots, payment links; each optional |
| `caldav.json` | The owner's CalDAV calendar of open slots, in place of `calendar.json`; secrets as `env:VAR` or `file:PATH`; optional |
| `approvals.json` | The owner's amount thresholds, such as `{"place_order": 20000}`; optional |
| `data/site-log.db`, `data/host/`, `data/history/` | The site log, the `DirectoryHost`, every saved version |
| `data/keys/` | `release.jwk`, `card.jwk` (and `receipt.jwk`) as private JWKs, made on first start, owner-only; never the root |
| `data/kernel.db`, `data/edge.db` | The kernel's stores (charters, KPIs, ladder, signals, agenda, approvals, devices, runs) and the edge's, with the quotes, orders, bookings, rate-limit windows, the back office's drafts and the owner's alarms |
| `data/audit.jsonl` | The audit trail (`adk-core`'s `FileAuditRecorder`): the pipeline's, the edge's, payments', the update engine's, the alarms', the review's and the signals' records, and `adk-rust awp keys`' |
| `data/updates/` | `batches.jsonl` and `feeds.json`, read by `adk-rust site batches` and `site feeds` |

The owner signs at a terminal: `adk-rust awp keys rotate` makes the root and the key set,
`adk-rust awp keys delegate --scope release` and `--scope card,receipt` delegate the keys the
site made. Until the release key is delegated, every build fails the gate's
`release-delegation` check, which names those commands (D-11, A-I5).

| Part | Runs |
|------|------|
| `UpdateEngine` | On the runtime's pipeline, driven when `next_due` says; each held delivery or item is an approval request in the owner's inbox — a decision card on the phone — and the answer releases or discards it (A-I23, A-I27); what it holds is kept in `data/updates/held.json`, so a card answered after a restart still decides its change |
| `ExperimentRunner` | Its daily look as the kernel run `website.experiments`, on a cron trigger |
| `KpiTracker` | With the beacon, commerce, spend and agent-search sources; AEO with search providers |
| `ReviewComposer` | With the alarms, the experiments and the updates; the weekly review when the owner has a channel |
| Concierge | With a model, `edge::concierge` over the edge's own public tools, through `AwpEdge::public_agent` |

A restart keeps the charter, the jobs' levels, the signals, the approvals, the paired device,
the quotes and orders, the bookings, the back office's drafts, the open, deferred and waiting
alarms and the edge's rate-limit windows, and continues after the release the host serves; no
second setup code is offered.

## Incidents and alarms

| Step | What happens |
|------|--------------|
| Probe | `GuardianProbe` re-proves the domain, probes the host and the origin, records `site.drift.detected` |
| Incident | One incident per drift: status `incident`, one P1 alarm repeated until a person acknowledges it |
| Rollback | To the last good signed release: the Guardian alone when the owner pre-authorised it (a standing rule), otherwise the owner's decision |
| Exit | Only a person closes the incident |

| Urgency | Delivery |
|---------|----------|
| P1 | At once on the P1 channels, quiet hours or not; repeated until acknowledged |
| P2 | On the owner's chosen channel outside quiet hours; the weekly review on a volunteer-run site |
| P3 | The weekly review |

## Experiments and measurement

| Step | What happens |
|------|--------------|
| Start | `ExperimentRunner::start` maps the plan onto the charter: the primary KPI's metric and direction, each guardrail with a tolerance from its KPI baseline; the analysis is fixed |
| Build | `ExperimentGenerator` wraps the site's generator: the variant is `/_exp/{id}/…`, `noindex`, its canonical link naming the control; the pages load `/_awp/experiments.js` |
| Count | The script assigns a tab by a stable hash of a random bucket kept in `sessionStorage`, and sends one exposure and one event per goal to the edge's beacon; no cookie, no identifier |
| Look | Each day the runner adds the new whole days to the totals and analyses them with `ExperimentAnalyzer` |
| Stop | A guardrail breach pauses the job before its next action, lowers it one level and keeps the control (A-I15); `RepublishOnStop` serves the control alone at once |
| Review | `ReviewComposer::with_experiments` puts each experiment's decision and evidence in the weekly review |

`measurement` gives the KPI tracker its non-driver sources: `BeaconKpi` (`beacon`),
`HostLogs` (`host_logs`), `CommerceKpi` over `MeteredActionLedger` (`commerce`),
`SpendLedgerKpi` over `MeteredSpendLedger` (`spend`), `AgentSearchKpi` over `EdgeMeter`
(`agents`) and `AeoKpi` over `AeoTracker` (`aeo`).

See [the docs page](../docs/official_docs/deployment/adk-website.md) and
[experiments and measurement](../docs/official_docs/deployment/website-experiments.md).
