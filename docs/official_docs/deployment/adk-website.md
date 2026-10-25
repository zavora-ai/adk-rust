# The AWP website product

`adk-website` is a prebuilt AWP 2.0 website (spec 3.5 Part C, §7.16–§7.24). It onboards its
owner, builds or migrates itself, passes its own launch gate, and then works towards the
owner's goals with a team of eight agents inside its charter's authority, budget and
guardrails. It holds no framework code: the harness's `website` feature, the kernel's
`charter` feature, `adk-site` and `adk-payments` supply everything it runs.

> **Note:** `adk-website` is **Experimental**. Its API may change while spec 3.5 lands.

```toml
[dependencies]
adk-website = "3.0.0"
```

## Site kinds

The kind is the first fact of the charter. Discovery proposes one — a WooCommerce shop, a
`.go.ke` or `.gov.uk` domain, a blog's posts, a donation plugin — and the owner confirms it.
`kinds::template(kind)` returns the kind's goals and KPIs, its agents, capabilities and
rules; `KindTemplate::draft_charter` turns them into charter version 1.

| Kind | Agents | Rules the template adds |
|------|--------|--------------------------|
| Personal | Concierge, Steward, Growth, Guardian | Smallest budget; no advertising or sales jobs; private about the owner |
| Business | All eight | The lead, nurture and handoff loop |
| E-commerce | All eight | Payment policy, product feeds, fraud and refund guardrails |
| Government | Concierge, Agent desk, Inbox, Steward, Growth, Guardian | Editor and publisher approval chain; WCAG 2.2 AA; every official language; no third-party scripts or trackers; no advertising or profit KPIs; records retention; AI disclosure on every reply |
| Nonprofit | All eight | Donor consent and receipts; tax-receipt fields |
| Education | Concierge, Agent desk, Inbox, Steward, Growth, Guardian, Revenue | Safeguarding; no profiling of minors; learning platforms stay outside |
| Media | Concierge, Steward, Growth, Guardian, Revenue | Content-usage terms; corrections policy; never generated news |
| Health | Concierge, Agent desk, Inbox, Steward, Growth, Guardian | No clinical advice; special-category data; emergency routing |
| Community | Concierge, Steward, Growth, Guardian, Revenue | Low budget; only P1 alarms interrupt |
| Events | All eight | Date-driven freshness; wind-down and archive |

Every draft starts each job at Observe or Propose for the first week; onboarding itself acts
only with the owner's approval of each change. No KPI has a target until it has a baseline,
and targets are the owner's (A-I14).

## Onboarding

| # | Stage | Plan steps | Gate |
|---|-------|-----------|------|
| 1 | Start | `start` | — |
| 2 | Discover | `discover` | — |
| 3 | Protect | `protect` | the owner approves the freeze |
| 4 | Interview | `interview` | — |
| 5 | Charter | `charter_draft`, `charter_sign` | every required signer signs |
| 6 | Build | `build` | — |
| 7 | Launch gate | `gate` | — |
| 8 | Connect and deploy | `connect`, `deploy` | the owner grants each connection and approves going live |
| 9 | Run | `run` | — |

| Part | What it does |
|------|--------------|
| `onboarding::SetupGate` | A fresh site accepts no onboarding input until its setup code, written to the logs, pairs the owner's device through the kernel's pairing (A-I20) |
| `onboarding::plan` | The nine stages as plan-graph steps built from the harness's onboarding steps |
| `onboarding::discovery::Discovery` | An existing site (a crawl by default; `WordPressRest`, `Wxr` or `IsolatedReplay` with `with_source`), an online presence, or nothing; a migration converts, quarantines and converts again without the held subjects |
| `onboarding::build::LivePreview` | Rebuilds the About page from the owner's `Known` facts after each answer and publishes it to the host's preview |
| `onboarding::build::SitePlan`, `stage_release_one` | Release 1: every page names the charter goal it serves, the About page claims only `Known` facts, the legal pages cover the charter's jurisdictions, every language is drafted |
| `onboarding::cost::estimate` | The monthly running cost from each agent's model runs, the owner's model prices, hosting and the domain |
| `onboarding::RunPlan` | The 30/60/90-day plan: Propose for the first week, the first weekly review on day 7, and the promotions to each job's starting level, which only the owner decides |

Discovery records what it finds as `Assumed`, citing the page it came from; the owner's own
words make a fact `Known`. Keeping the current design gates a migration's launch on visual
parity under 0.05; a new theme gates on URL coverage, text and metadata, and the owner
approves the look from the preview.

## The team

The Steward leads the back office (Steward, Growth, Revenue, Guardian, Finance) as one
`TeamSpec`; the public agents (Concierge, Agent desk, Inbox) are woken by their own channels
and reach the back office only through typed signals. `SiteTeam::build` makes each agent a
`HarnessAgent` with its partition's kit, the ladder's mode and run budget for its jobs, and
its jobs' playbooks under `.skills/` in its workspace.

`jobs::processes` turns the jobs into kernel triggers: `site.probe` every five minutes and
`site.digest` on Mondays run without a model through `workflows::SiteWorkflows`; the other
site processes run their job's agent with its playbook.

## Incidents, alarms and the weekly review

| Step | What happens |
|------|--------------|
| Probe | `GuardianProbe` re-proves the domain claim, probes the host and the origin, and hands the report to `IncidentPath` |
| Incident | One incident per drift: status `incident`, one P1 alarm |
| Rollback | To the last good signed release: the Guardian rolls back alone when the owner has pre-authorised it (its `protect` job at "act, reversible only", a standing rule the owner's decision created); at "act with approval" the call is held; otherwise the rollback is the owner's to approve |
| Exit | Only a person closes the incident and restores `operational` |

| Urgency | Delivery |
|---------|----------|
| P1 | At once on the P1 channels, inside quiet hours too; repeated every 15 minutes until a person acknowledges it |
| P2 | On the owner's chosen channel, deferred to the end of quiet hours; the weekly review on a volunteer-run site |
| P3 | The weekly review |

`review::ReviewComposer` composes the weekly review from the stores: KPIs against baselines
and targets with trends, each agent's work, cost, revenue and profit and loss from the wallet,
experiments, incidents, leads, the promotions the jobs earned and the demotions with their
reasons, the alarms that waited, and the owner's questions. It sends a one-line summary to
the owner's channel and returns the document for the web home.

## The agent edge

`edge::SiteEdge::build` serves the kind's capabilities on `adk-server`'s agent edge (spec 3.5
§7.9) with the packs' handlers. The edge runs the governed path before a handler sees a
call; the handlers keep leads, contacts, quotes and orders, and file signals into the back
office's agenda through the kernel's `SignalSink`, each text field screened (A-I3).

| Adapter | Joins |
|---------|-------|
| `KernelSignals` | The edge's signals to the back office's agenda |
| `ReleaseSearch` | `search_site` to the index of the release's Markdown twins |
| `InboxThreads` | In-thread replies to the inbox drivers |
| `SiteSubjectData` | Consent export and erasure to the leads and the contact book |
| `EdgeAnnouncer`, `DeferredAnnouncer` | Each release to `site.release.published` and `catalog.changed` on the edge's events |
| `ReceiptEntrySigner` | Stream entries to the edge's delegated `receipt` key |

`edge::site_router` serves the edge's routes with the release the host serves behind them,
each file with its release media type. `edge::consent_config` asks for the consent scopes the
kind's capabilities name.

| Incident step | The edge |
|---------------|----------|
| Drift opens an incident | Read-only mode: non-read capabilities answer `503 read_only` with the signed status |
| The Guardian's `edge_read_only` | Read-only for a reason, or back with `resume: true` |
| A person closes the incident | Serving again, status `operational` |

## The contract

`contract::site_documents` signs the contract, the key set and the public terms with the
site's root; `contract::add_to_project` puts them in the project every release is built from.

| Key | Delegated | Signs |
|-----|-----------|-------|
| Root | — | The contract and the delegations; anchored by the `_awp-root` TXT record from `SiteKeys::root_record` |
| Release | `release` | The site log's checkpoints; the same seed as `adk-site`'s `Ed25519ReleaseSigner` |
| Edge | `card`, `receipt` | The Agent Card, the status, consent receipts and stream entries |

`adk_awp::verify::Verifier` verifies a served site end to end: the contract, the anchored
root, the delegations, the signed Agent Card, the log, the release and the public terms.

## Deployment

| Shape | Content | Edge and kernel | Files `deploy::plan` writes |
|-------|---------|-----------------|-----------------------------|
| `local` | `DirectoryHost`, or any host driver | `adk-rust up --install`, then `adk-website start .` | None |
| `container` | Any static host | One process serving the edge publicly and the kernel on loopback | `Dockerfile`, `.dockerignore`, `scripts/build-image.sh` |
| `managed` | Any static host | The managed agent runtime through `adk-deploy` | `adk-deploy.toml` |
| `agency` | Per-site hosts | `adk-website agency agency.toml` | `agency.toml` |

Each plan carries the month's running cost estimate before deployment. `deploy::serve` starts
the kernel with device pairing on its control plane, writes the setup code to the logs, and
serves the edge; before the launch `/` answers the holding page. `deploy::serve_agency` runs
many sites from one harness:

| Layer | Isolation (A-I22) |
|-------|-------------------|
| Kernel | Each site's partitions carry the site as tenant and deny crossings; a request from one site's back office into another's — the agency coordinator's too — is denied and recorded as a crossing |
| Edge | `AgencyRouter` sends each request only to the edge of the site its host names; any other host is refused with `404` and recorded as `awp.request.refused` |

## Audit records

| Record | Written by |
|--------|-----------|
| `website.onboarding.started`, `website.onboarding.stage`, `website.onboarding.completed` | `OnboardingProgress` |
| `website.charter.drafted` | `onboarding::draft_charter` |
| `website.alarm.raised`, `website.alarm.acknowledged` | `AlarmRouter` |
| `website.review.sent` | `ReviewComposer::send` |
| `site.status.changed`, `site.rollback` | `IncidentPath` |
| `awp.request.refused` | `deploy::AgencyRouter` |
