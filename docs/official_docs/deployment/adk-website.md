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

Reviews discovery finds join `quotes::ReviewQuotes` (`data/reviews.json` in a served site),
none approved; the onboarding view asks the owner about each one with a public reviewer's
name, and `quotes::QuotedReviews` — a launch-gate extension over them — fails a release that
quotes an anonymous review, one the owner has not approved, or one without its reviewer's
name on the page.

`ui::WebsiteDesk` saves the paired device, the onboarding answers, the charter under review
and the editor's drafts to its `ui::DeskStore` after every command (`FileDeskStore` at
`data/desk.json` under `adk-rust website new`) and reads them back with
`WebsiteDesk::restore`, so the owner resumes onboarding after a restart from the device the
setup code paired.

## The team

The Steward leads the back office (Steward, Growth, Revenue, Guardian, Finance) as one
`TeamSpec`; the public agents (Concierge, Agent desk, Inbox) are woken by their own channels
and reach the back office only through typed signals. `SiteTeam::build` makes each agent a
`HarnessAgent` with its partition's kit, the ladder's mode and run budget for its jobs, and
its jobs' playbooks under `.skills/` in its workspace, and compiles the back office:
`BuiltTeam::compile` returns it as one runner root over the built agents, the Steward's
delegate edges included. `BuiltTeam::team_runner` runs that root under the Steward's
harness; each member it delegates to runs under its own harness's policy.
`BuiltTeam::agent_runner` runs one agent alone, for the kernel's process of one of its
jobs.

Each job binds the tools it acts with, so the owner's promotion grants their standing rules:

| Job | Tools beyond the drafting tools |
|-----|---------------------------------|
| `protect` | `site_rollback`, `edge_read_only` (A-I11) |
| `social` (review replies) | `site_publish`, `social_post` |
| `nurture` | `adk-drivers`' `email_send_sequence_step`, through `leads::nurture_tools` |
| `handoff` | `lead_handoff`: `alarms::handoff_tool_for` sends the owner one P2 card per lead, once its score reaches the threshold |
| `experiments` | `experiment_start`, `experiment_report`: `experiments::experiment_tools` over the runner |

With `TeamKit::wallet`, the Finance agent pays from the owner's wallet: `wallet::finance_tools`
gives it `wallet_pay` and `wallet_status` with the payment scope, and no other agent holds
them; the wallet's mandates and the governed path decide each payment (spec 3.5 §7.14,
A-I16).

`jobs::processes` turns the jobs into kernel triggers: `site.probe` every five minutes,
`site.watch` daily and `site.digest` on Mondays run without a model through
`workflows::SiteWorkflows`; the other site processes run their job's agent with its playbook.

### Leads and nurture

| Part | Does |
|------|------|
| Consent | The charter's public terms and the edge ask for `contact` and, where the kind nurtures its leads, `nurture`; an enquiry carries every receipt the visitor granted (`AWP-Consent` lists them) |
| `leads::SqliteLeadRecords` | The contact book beside the edge's leads in `data/edge.db`: who wrote, with which receipts, how much nurture they received, which leads were handed off; erasure reaches it |
| `leads::Leads` | The email outbox's `ConsentCheck` from the edge's receipts, so a revocation stops the next send; scores and the one handoff per lead |
| `leads::LeadScorer` | 20 for `contact`, 10 for `nurture`, 15 per qualifying answer (when, how many, budget), 5 per nurture step up to three; a lead at the charter's `handoffThreshold` (70 when it sets none) is ready for the owner |
| `leads::nurture_tools` | The outbox's sequence and newsletter sends, refused inside the owner's quiet hours, each send returning the recipient's lead scores |

### Upkeep and the publish delay

`upkeep::UpkeepCheck` is the hosting job's daily watch (`jobs::WATCH_WORKFLOW`): it reads
the edge's `Cargo.lock` against an `AdvisorySource` — `AdvisoryDatabase` over a local clone
of the RustSec advisory database — and the certificate from a `CertificateSource`, and files
each upgrade or renewal once as a decision card for the owner, with a P2 alarm (P1 for a
certificate within three days). `publish_delay::PublishDelay` keeps a page the site drafts on
its own under `freshness`, `seo` or `aeo` off the site for the charter's `publishDelayHours`
(24 by default): the draft must be scheduled at least that far ahead, and it goes live at
that time once the owner has approved it. The charter card "When the site asks you" shows
the handoff threshold, the publish delay and the amount thresholds, defaults included
([The charter](charter.md#the-owners-settings)).

### The owner's channel

`channel::OwnerChannel` is what a served site's chat route hands the photos of a paired
person's message to ([Chat on a served site](#chat-on-a-served-site)): it runs them through the back office's media intake into the Steward's draft, drafts each
alt text from its caption, and asks the owner on the thread to confirm. `yes` confirms them
all; `2: text` rewrites the second; the confirmation reaches the item once it is scheduled or
published. Photos that arrive after the Steward scheduled the draft join the scheduled item,
which is not live yet. A message from an account no one paired adds nothing (A-I24).

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
reasons, the alarms that waited, and the owner's questions. It sends a summary in a few plain
sentences to the owner's channel and returns the document for the web home; the composer is
`Clone`, so the home's review view and the weekly digest compose from the same stores.

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
| `ReceiptKey` | Commerce receipts to the edge's delegated `receipt` key, for `adk-payments`' receipt hook |

`SiteEdgeParts::payments` takes the owner's payment desk, such as `edge::ap2::Ap2Desk`, and
`SiteEdgeParts::thresholds` the owner's amount thresholds, which hold large orders, bookings
and payments for the owner ([Writing capability packs](capability-packs.md#amount-thresholds)).

`edge::concierge` builds the Concierge over the edge's own public tools with `adk-harness`'
`website_public_served`; it is offered those tools and nothing of the harness's own.
`edge::site_router` serves the edge's routes with the release the host serves behind them,
each file with its release media type. `edge::consent_config` asks for the consent scopes the
kind's capabilities name.

| Incident step | The edge |
|---------------|----------|
| Drift opens an incident | Read-only mode: non-read capabilities answer `503 read_only` with the signed status |
| The Guardian's `edge_read_only` | Read-only for a reason, or back with `resume: true`; the catalogue's reversible incident act, pre-authorisable at "act, reversible only" |
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

### A served site

`deploy::serve` opens the project in the layout `adk-rust site` uses and starts one
`deploy::RunningSite` over the kernel's durable stores in `data/kernel.db`:

| Part | Wiring |
|------|--------|
| Keys | `data/keys/release.jwk` signs the site log; `card.jwk` (and `receipt.jwk`) sign the Agent Card, the status, consent receipts and stream entries. No root key is kept in the project |
| Publishing | Refused until `well-known/awp/keys.json` delegates the release key: the gate's `release-delegation` finding names `adk-rust awp keys rotate` and `adk-rust awp keys delegate`. The gate's full tier — `launch_gate_run` and the update engine's scheduled checks — also runs `awp verify` against the preview of the next release (`deploy::checks::preview_verification`): the build staged over the site log the host serves, the Agent Card read from the served edge, the DNS root anchor checked against the `_awp-root` record the project's key set names; it reports `GateCheck::AwpVerify` |
| Updates | `UpdateEngine` over the runtime's pipeline, the edge's live store and a stream lane signed by the receipt key; feeds from `feeds.json`; `UpdateDriver` ticks it when due and journals to `data/updates/`; held deliveries and items become approval requests (decision cards) and survive a restart in `data/updates/held.json`; `GET /awp/v2/stream` serves `stream_since` |
| Experiments | `ExperimentGenerator` around the built-in generator; `ExperimentRunner` with `BeaconKpi`, the job pauses, the ladder, the alarms and `RepublishOnStop`; its trigger fires through the kernel's watermarked cron source for the expression |
| Measurement | `MeteredActionLedger` and `EdgeMeter` on the edge, `MeteredSpendLedger` for agents' spend, and the tracker's sources |
| Channels | In-thread replies through the channel a message came in on: `SiteOptions::repliers`, the site's email outbox, and the chat channels of the owner's sinks (`inbox::ChannelReplier`); the calendar from `caldav.json` (the owner's CalDAV calendar, `edge::caldav::CalDavCalendar`) or `calendar.json`, payment links from `payments.json`, or `SiteOptions` (`Ap2Desk`, `TokenDesk` or `PaymentRails` as `SiteOptions::payments`); refused without them; the owner's amount thresholds from the charter's `amountThresholds`, else `approvals.json` ([Writing capability packs](capability-packs.md#bookings)) |
| Public agents | With `SiteOptions::model`, `edge::concierge` mounts the Concierge through `AwpEdge::public_agent`: the website preset in the public partition over the edge's own tools — the declared capabilities (a declared `search_site` among them), `reply_in_thread` and the edge's `emit_signal` — so the edge's public policy admits every call. Where the kind runs one, `inbox::SiteInbox` is the Inbox over the same tools and policy: it reads the site's email while the site serves (the cursor kept in `data/inbox/email.cursor`), and `SiteInbox::receive_inbound` reads a chat message a `ChannelSink` adapter received from an account no person paired — a paired account's message, by `SiteOptions::channel_bindings`, is the owner's (A-I24). Each reply answers the message's own thread; a flagged or refused message the Inbox filed no signal for gets the site's `inbound_message` signal, with none of its text (A-I3) |
| Back office | With `SiteOptions::model`, the kernel hosts the back office (`deploy::back_office`): the Steward's target runs the team, every agent its jobs' processes on their signals and schedules, under the owner's standing rules in `data/kernel.db` — what `adk-rust website jobs promote` grants takes effect while the site serves — with held calls in the owner's approval inbox; the edge's signals wake them, and the kernel defers each proactive run inside the charter's quiet hours (an agency's kernel holds none). Revenue gets the nurture sends (`SiteOptions::email`) and the handoff, Growth the experiment tools, Guardian `edge_read_only`, Finance the wallet (`SiteOptions::wallet`); agent sessions are kept in `data/kernel/sessions.db` |
| Upkeep | With `SiteOptions::advisories` or `SiteOptions::certificate`, the daily `site.watch` runs `UpkeepCheck` |
| Learning | `learning::PlaybookLearning` runs weekly after the review (`website.steward.learning`): an experiment its test decided, or a KPI short of the owner's target two weeks running, proposes a playbook revision on the owner's decision card (at Observe, in the report only); an accepted or edited revision is the playbook's next version, written into its agents' workspaces ([Playbook Learning](website-learning.md)) |
| Repository | With a `repository.json`, `repository::RepositoryEntry` serves the content repository's signed hooks at `/hooks/{name}` on the edge: a push from an authorised committer becomes a kernel run that reads the changed content files at the commit and proposes a release on the owner's decision card; anyone else's push files one signal ([Publishing from Git](website-repository.md)) |
| Guardian | With `SiteOptions::probe` and the owner's domain claim — `ProbeOptions::claim`, else `data/claims/{domain}.json`, which `adk-rust site migrate` keeps — `site.probe` runs `incident::GuardianProbe` every five minutes: the claim is proven again by DNS, the host and the origin are read against the signed releases, and drift opens an incident on `RunningSite::incidents` (status `incident`, the edge read-only, one P1 alarm, a rollback plan the owner approves) |

The `adk-website` binary's `start` and `adk-rust website new` read the project's
configuration files into `SiteOptions` with `deploy::config::configure`; secrets are
`env:VAR` or `file:PATH`, never the secret itself (A-I19):

| File | Sets |
|------|------|
| `email.json` | `SiteOptions::email`: an IMAP and SMTP account (`adk_drivers::email::ImapSmtpEmail`), the address it sends from, and the sender policy bulk sends wait for |
| `upkeep.json` | `SiteOptions::advisories` (the edge's `Cargo.lock` against a RustSec advisory database checkout) and `SiteOptions::certificate` (the origin's certificate) |
| `wallet.json` | `SiteOptions::wallet`: the owner's `WalletPolicy` and the providers that bill the owner's own accounts, each a `provider_billing:{provider}` rail whose charges are recorded in `data/wallet/{provider}/charges.jsonl` and reconciled against the `invoice.jsonl` the owner exports there; payments are kept in `data/wallet.db`. The wallet holds no funds of its own here |
| `data/claims/{domain}.json` | `SiteOptions::probe`, proven with the system's resolver |
| `repository.json` | Read by `RunningSite::open`: the content repository's branch, source, signed hooks and authorised committers ([Publishing from Git](website-repository.md)) |
| `channels.json` | Read by `deploy::config::chat` for `adk-rust website new`, which builds the adapters into `SiteOptions::chat` ([Chat on a served site](#chat-on-a-served-site)) |

```json
{
  "imap": { "host": "imap.mail.example", "port": 993, "security": "tls" },
  "smtp": { "host": "smtp.mail.example", "port": 587, "security": "starttls" },
  "username": "hello@juniper.example",
  "password": "env:JUNIPER_MAIL_PASSWORD",
  "senderPolicy": { "dkimSelector": "mail", "spfMechanism": "include:_spf.mail.example" }
}
```

An agency's sites share one set of options, so `adk-website agency` reads none of these
files.

### Chat on a served site

With `SiteOptions::chat`, the served site runs one channel gateway on its kernel over the
chat platforms the host hands it (spec 3.5 §10 X12). `adk-website` cannot build the adapters,
which are `adk-surface`'s, so `adk-rust website new` builds them from the project's
`channels.json` (`website_cli`), each secret `env:VAR` or `file:PATH`:

```json
{
  "whatsapp": {
    "phoneNumberId": "106540352242922",
    "accessToken": "env:WHATSAPP_ACCESS_TOKEN",
    "appSecret": "env:WHATSAPP_APP_SECRET",
    "verifyToken": "env:WHATSAPP_VERIFY_TOKEN",
    "template": { "name": "decision_waiting", "language": "en_US" }
  },
  "telegram": { "token": "env:TELEGRAM_BOT_TOKEN" },
  "slack": { "appToken": "env:SLACK_APP_TOKEN", "botToken": "env:SLACK_BOT_TOKEN" },
  "discord": { "token": "env:DISCORD_BOT_TOKEN" },
  "matrix": { "homeserver": "https://matrix.example.org", "accessToken": "env:MATRIX_ACCESS_TOKEN" }
}
```

The gateway's route (`deploy::chat::SiteChat`) tells two kinds of sender apart:

| Sender | What happens |
|---|---|
| An account paired to the owner or staff | A turn of the Steward as that person, in a session whose entry point is the paired device, so its drafts enter through it (A-I24); decision cards ring the account. Photos the message carried — every adapter downloads them — wait for the draft the turn makes, or join the person's newest draft when no turn runs, through `channel::OwnerChannel`, which asks for their alt text in the thread; `yes` or `2: text` answers it and starts no turn |
| Anyone else | The Inbox reads the message as a visitor's, and answers in the thread; without an Inbox the site files an `inbound_message` signal. No pairing code, no turn, and no media kept |
| Anyone else typing `/pair` (Slack's and Discord's `/pair` command) | The gateway's pairing code, which the owner accepts with `adk-rust pair accept` against the site's kernel |

Pairings are kept in `data/kernel.db` (`KernelStores::channel_bindings`), unless
`SiteOptions::channel_bindings` names another store. The site's edge serves WhatsApp's
webhook at `/channels/whatsapp` (`deploy::chat::WHATSAPP_PATH`): give WhatsApp the edge's
public URL with that path and the verify token. An agency runs no chat gateway.

| Store | Survives a restart |
|-------|--------------------|
| Charters, KPIs, autonomy levels, signals | `adk_kernel::charter::Sqlite*Store` |
| Agenda, partitions, devices, cron watermarks | `adk_kernel` SQLite stores |
| Runs (weekly reviews among them), approvals, action and spend ledgers | `adk_session` SQLite stores |
| Leads, consent, held calls, idempotency | `adk_server::awp::stores::SqliteEdgeStores` in `data/edge.db` |
| The contact book, nurture counts and handoffs | `leads::SqliteLeadRecords` in `data/edge.db` |
| The owner's standing rules | `adk_harness::SqliteStandingRuleStore` in `data/kernel.db` (`KernelStores::rules`) |
| Reviews the site may quote | `data/reviews.json` |
| The desk's work in progress | `data/desk.json`, under `adk-rust website new` |
| Retained events, catalogue pages, consent references, pending deliveries, beacon counts | The same `SqliteEdgeStores`, as the edge's event log and beacon store; the deliveries a stopped process left resume once their lease passes |
| Measurement counts (`MeteredActionLedger`, `EdgeMeter`, `MeteredSpendLedger`, host logs, AEO) | `SqliteEdgeStores` in `data/edge.db`, under the namespace `{site}#measurement` |
| The edge's rate-limit windows | The same `SqliteEdgeStores`, through `EdgeLimits::store` |
| Quotes, orders and the bookings of `calendar.json`'s slots | `edge::commerce::SqliteCommerceStore` in `data/edge.db`, with `edge::commerce::StoredCalendar`; `SiteEdge::build_with_commerce` takes the store |
| The back office's drafts, charter drafts, charter signing and promotion requests | `adk_harness::website::SqliteSiteDraftStore` in `data/edge.db`, read back by `SiteRuntime::restore_drafts`; a preview and the votes on it are made again |
| The owner's open, deferred and waiting alarms | `alarms::SqliteAlarmStore` in `data/edge.db` (`AlarmRouter::with_store`) |
| Pushes the content repository's hooks delivered, and what became of each | `data/repository/pushes.json` |
| Playbook revision proposals; accepted playbook versions | `data/learning/proposals.json`; `data/playbooks/{playbook}.json` |
| The audit trail | `data/audit.jsonl`, through `adk_core::audit_log::FileAuditRecorder`: the pipeline's, the edge's, the update engine's, the alarms', the review's, the experiments', the ladder's, the tracker's and the signals' records |

## Audit records

| Record | Written by |
|--------|-----------|
| `website.onboarding.started`, `website.onboarding.stage`, `website.onboarding.completed` | `OnboardingProgress` |
| `website.charter.drafted` | `onboarding::draft_charter` |
| `website.alarm.raised`, `website.alarm.acknowledged` | `AlarmRouter` |
| `website.review.sent` | `ReviewComposer::send` |
| `site.status.changed`, `site.rollback` | `IncidentPath` |
| `awp.request.refused` | `deploy::AgencyRouter` |
| `awp.payment.created`, `awp.payment.settled`, `awp.payment.rejected` | `edge::handlers::PaymentHandler` (`with_audit`; the edge's trail on a served site) |
| `site.content.drafted`, `approved`, `scheduled`, `published`, `corrected`, `withdrawn`; `site.release.built`, `signed`, `published`, `refused` | `adk-site`'s `PublishingPipeline` (`PublishingPipelineBuilder::audit`; `SiteRuntimeBuilder::audit` passes its trail) |
| `awp.keys.rotated`, `awp.keys.delegated` | `adk-rust awp keys`, into the project's `data/audit.jsonl` |

Every release runs in a `site.release` span (`observability::spans::SITE_RELEASE`) with its
sequence, source and items, and every signal the edge files in a `kernel.signal` span
(`observability::spans::KERNEL_SIGNAL`) with its kind, id and emitter.
