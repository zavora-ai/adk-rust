# Experiments and measurement on an AWP website

`adk-website` runs web experiments against the charter's KPIs and guardrails, and gives the
KPI tracker the measurement sources that are not drivers (spec 3.5 §7.12, A-I15, S11). The
analysis is `adk-eval`'s `ExperimentAnalyzer` ([Web Experiments](../evaluation/experiments.md));
this page covers how a site runs one: variants as static files, counting through the edge's
beacon, the daily look, the stop, and the weekly review.

> **Note:** `adk-website` is **Experimental**. Its API may change while spec 3.5 lands.

```toml
[dependencies]
adk-website = "3.0.0"
```

## How an experiment runs

1. The Growth agent's `experiments` job writes an `ExperimentPlan`: the control page, the
   variant's changes, the KPI to move and, optionally, the guardrails to monitor.
2. `ExperimentRunner::start` maps the plan onto the charter in force and fixes the analysis
   before the first look. It refuses while the job is paused, an id already used on the site,
   or a page another running experiment holds.
3. The next release carries the variant as its own static file. `ExperimentGenerator` wraps
   the site's generator; it is the generator the site runtime builds with.
4. In the browser, `/_awp/experiments.js` assigns the tab to an arm and counts exposures and
   goals through the edge's beacon.
5. Once a day `ExperimentRunner::look_all` reads the whole days since the last look, adds them
   to each experiment's totals and analyses the totals.
6. A final decision stops the experiment. A guardrail breach also pauses the job before its
   next action and lowers it one level; the control is kept.
7. The weekly review shows each experiment that ran in the week, with its decision and the
   analyzer's evidence block.

## The charter as an experiment

| Charter | Experiment |
|---------|------------|
| The plan's `primary_kpi`, a KPI id | The primary metric: the KPI's `metric`, as a proportion of exposures |
| KPI `direction` `up` / `down` | `MetricDirection::HigherIsBetter` / `LowerIsBetter` |
| The plan's `guardrails`, or every guardrail whose `jobs` list the plan's job or is empty | One guardrail per metric |
| Guardrail `breach` `above` / `below` | `LowerIsBetter` / `HigherIsBetter` |
| Guardrail `threshold` with its metric's baseline `b` | `Tolerance::Relative(t/b − 1)` above, `Relative(1 − t/b)` below |
| No baseline, or one already past the threshold | `Tolerance::Absolute(0.0)`: any proven harm breaches |

A charter guardrail is an absolute limit on a site metric; an experiment guardrail limits how
much worse the variant may be than the control. The mapping keeps the charter's headroom:
with unsubscribes at a 1.5% baseline and a 2% threshold, the variant may raise them by at most
a third, the rise that would carry the site to its threshold once adopted. The baseline is the
KPI's in the charter, else the one the KPI tracker locked (`with_kpis`).

## Variants as static files

| File | Content |
|------|---------|
| The control, such as `/index.html` | The page as built, plus `<script src="/_awp/experiments.js" data-awp-experiment="{id}" data-arm="a" data-allocation="…" data-variant="/_exp/{id}/" data-beacon="…">` |
| `/_exp/{id}/index.html` | The control with the plan's `Replacement`s applied, `<meta name="robots" content="noindex">`, and the script with `data-arm="b"`; its canonical link still names the control |
| Each goal page | The script with `data-arm=""`: it counts goals and assigns nothing |
| `/_awp/experiments.js` | The client script, while any experiment runs |

The page path ends in `/` or `.html`. A replacement changes text and markup only: the plan
refuses scripts, frames, embeds, forms, style sheets, `meta` and `base` elements, event
handlers and script URLs. A control page that no longer holds a replacement's text is left
out of the build, with a warning, until it does again. Every file is part of a signed release
(A-I4); a stopped experiment's variant and tags leave the next build.

## Assignment and counting

The script draws a random bucket per tab session and keeps it in `sessionStorage`; it never
leaves the browser. The arm is `fnv1a("{id}:{bucket}") / 2³² < allocation`, the hash
`adk_website::experiments::bucket_hash` computes over UTF-16 code units, so an edge that
assigns with `assign` agrees with the browser. A tab in the variant arm is sent to the
variant's file, its query and fragment kept.

| Event the script sends | When |
|------------------------|------|
| `exp.{id}.{arm}.exposure` | Once per tab session and arm |
| `exp.{id}.{arm}.{metric}` | Once per tab session, arm and metric, when a form marked `data-awp-goal="{metric}"` is submitted, a link or button so marked is clicked, or a page calls `awpExperiments.goal("{metric}")` |

A link that names the arm in its fragment, `#awp-exp={id}.{arm}`, attributes the visit to that
arm on a goal page; a nurture email's unsubscribe link carries it, and the fragment never
reaches a server. With script off, the page shows the control and measures nothing.

The edge's beacon counts each event per day as `events:{name}`, sets no cookie, keeps no
address and counts nothing under `Sec-GPC: 1`. The unit of analysis is an exposed tab session:
a goal reached is one event of the arm that tab saw, so every metric is a proportion of
exposures.

## Looks and decisions

| Decision | What the site does |
|----------|--------------------|
| `continue` | Keeps serving both arms |
| `adopt the variant` | Stops; the next build serves the control alone, and the job proposes the variant's change as content at its autonomy level |
| `keep the control — guardrail breached` | Pauses the job through the kernel's `JobControl` before anything else, records `website.job.paused`, emits `CharterEvent::JobPaused` (P2), lowers the job one level on the ladder, and stops (A-I15) |
| `keep the control — the variant is worse`, `sample limit reached` | Stops; the control is served alone |

A look reads only whole days, so a second look on the same day adds nothing, and a look that
fails changes nothing. The record — the plan, the fixed analysis, the cumulative counts, the
analyzer's `ExperimentState` and the latest report — is kept in an `ExperimentStore`
(`InMemoryExperimentStore`, or `FileExperimentStore` in the site's data directory) and
serialises to camelCase JSON for the web home. `RepublishOnStop` republishes the plan's
`item` through the site's publishing pipeline when an experiment stops, so the control is
served alone at once; the release goes through the launch gate and the charter's approval
chain like any other. Only a person resumes a paused job.

The weekly review shows one entry per experiment that ran in the week:

```text
- Experiment `headline` on / — Saying when the bread is baked lifts sign-ups (started 2026-09-07): stopped 2026-09-13, keep the control — guardrail breached: email.unsubscribe_rate; the experiments job is paused until you resume it, and the control is served.

  Experiment 'headline', look 6: keep the control — guardrail breached: email.unsubscribe_rate
  Primary newsletter.signups (higher is better): control 4.00% of 12000, variant 5.00% of 12000
    difference +1.00 pp, 95% interval [+0.18 pp, +1.82 pp]; relative lift +25.0%, 95% interval [+2.1%, +47.9%]
    always-valid p 0.0075 against alpha 0.05; z 3.74 against 3.06
  Guardrail email.unsubscribe_rate (lower is better, tolerance 33.3% relative): control 0.60% of 12000, variant 1.20% of 12000
    difference +0.60 pp, 90% interval [+0.26 pp, +0.94 pp]; relative lift +100.0%, 90% interval [+19.7%, +180.3%]
    harm beyond tolerance +0.40 pp, lower bound +0.02 pp; always-valid p 0.0743 against alpha 0.1000: breached
```

## Wiring it

```rust
use std::sync::Arc;

use adk_harness::website::{BackOfficeKit, SiteRef, SiteRuntime};
use adk_kernel::charter::{AutonomyLadder, CharterStore, KpiStore};
use adk_server::awp::AwpEdge;
use adk_site::generator::{BuiltinGenerator, SiteGenerator};
use adk_website::alarms::AlarmRouter;
use adk_website::experiments::{
    ExperimentGenerator, ExperimentPlan, ExperimentRunner, ExperimentStore, RepublishOnStop,
};
use adk_website::measurement::BeaconKpi;
use adk_website::review::ReviewComposer;
use adk_website::team::AgentRole;

/// The generator the site runtime builds with.
fn generator(site: &SiteRef, experiments: Arc<dyn ExperimentStore>, edge: &str) -> Arc<dyn SiteGenerator> {
    Arc::new(ExperimentGenerator::new(
        Arc::new(BuiltinGenerator),
        experiments,
        site.id,
        format!("{edge}/awp/v2/beacon"),
    ))
}

/// The runner: counts from the edge's beacon, pauses through the back office's job pauses.
fn runner(
    site: &SiteRef,
    charters: Arc<dyn CharterStore>,
    kpis: Arc<dyn KpiStore>,
    experiments: Arc<dyn ExperimentStore>,
    edge: &AwpEdge,
    kit: &BackOfficeKit,
    ladder: AutonomyLadder,
    alarms: Arc<AlarmRouter>,
    runtime: Arc<SiteRuntime>,
) -> Arc<ExperimentRunner> {
    Arc::new(
        ExperimentRunner::new(site.id, charters, experiments)
            .with_source(Arc::new(BeaconKpi::new(site.id, edge.beacon())))
            .with_kpis(kpis)
            .with_control(kit.pauses.clone())
            .with_ladder(ladder)
            .with_events(alarms)
            .with_stop_hook(Arc::new(RepublishOnStop::new(runtime, AgentRole::Growth.principal(site)))),
    )
}

async fn run(runner: Arc<ExperimentRunner>, composer: ReviewComposer) -> adk_core::Result<()> {
    runner
        .start(
            ExperimentPlan::new("headline", "/", "signups")
                .hypothesis("Saying when the bread is baked lifts sign-ups")
                .replace("<h1>Juniper Bakery</h1>", "<h1>Bread out of the oven at 5am</h1>")
                .item("pages/home/en"),
        )
        .await?;
    // Daily, or as the kernel run `website.experiments` on `runner.trigger(app, "0 30 3 * * *")`.
    runner.look_all().await?;
    // The weekly review carries each experiment's decision and evidence.
    let _composer = composer.with_experiments(runner);
    Ok(())
}
```

## Measurement sources

The KPI tracker reads each KPI's `source` by name. Each source serves one site and refuses
another's reads (A-I22).

| Source | Name | Metrics |
|--------|------|---------|
| `BeaconKpi` over `AwpEdge::beacon()` | `beacon` | `pageviews`, `pageviews:{path}`, `events:{name}`, `visitors` |
| `HostLogs` | `host_logs` | `requests`, `bytes`, `status:2xx` … `status:5xx`, `not_found`, `pageviews`, `pageviews:{path}`, `crawlers`, `agents`, `agents:{name}`, `machine_views`, `agents.machine_views` |
| `CommerceKpi` over `MeteredActionLedger` | `commerce` | `actions`, `actions:{capability}`, `actions:{capability}:{completed\|failed\|not_run}`, and `leads`, `sales` from the site's `CommerceOutcomes` |
| `SpendLedgerKpi` over `MeteredSpendLedger` | `spend` | `spend`, `spend.vendor:{vendor}`, `spend.agent:{agent}`, in USD |
| `AgentSearchKpi` over `EdgeMeter` | `agents` | `calls`, `calls:{capability}`, `held`, `agent_calls`, `agent_calls:{binding}`, `agent_calls:{capability}`, `agent_discovery`, `agent_discovery:{surface}` |
| `AeoKpi` over `AeoTracker` | `aeo` | `aeo.queries`, `aeo.citations`, `pages.answer_citations`, `aeo.citations:{provider}`, `aeo.top3`, `aeo.citation_rate` |

| Part | How it counts |
|------|---------------|
| `HostLogs::ingest` | Parses the Common, Combined and W3C extended (`#Fields:`) formats and adds each request to its day; it keeps no address, query string or referrer. User agents are claims: `agents:{name}` counts self-declared AI crawlers such as `gptbot` and `claudebot` |
| `MeteredActionLedger` | Wraps the edge's action ledger (`AwpEdge::builder(..).action_ledger(..)`) and counts each completed `non_idempotent` call by capability and outcome; the exactly-once guarantee is the inner ledger's (A-I7) |
| `MeteredSpendLedger` | Wraps the runtime's spend ledger and counts committed spend by vendor and agent; caps are the inner ledger's |
| `EdgeMeter` | Wraps the edge's audit recorder (`.audit(AuditTrail::new(meter))`) and counts `awp.capability.called` and `awp.capability.held` records, and the edge's `awp.request.verified` records of card and manifest fetches by surface; an agent call or discovery is one from a requester above `public`, which takes a verified credential (A-I6) |
| `AeoTracker::run` | Asks each `web_search` provider (`adk_harness::web::SearchProvider`) each question and records whether a result's host is the site's domain, and at which rank; it reads only, at Observe |

Counts are daily totals in a `BeaconStore`; a source answers `None` for a window that covers no
whole day. Search-console performance is `adk-drivers`' `SearchMetrics` (`search`), and the
wallet's spend, revenue, profit and costs per lead and per sale are `WalletKpi` (`spend`):
register one `spend` source, or rename `SpendLedgerKpi` with `named`.

## Limits

| Limit | Why |
|-------|-----|
| Metrics counted through the beacon are proportions | The beacon counts events, not per-unit values; a mean metric needs a source that reports sums and sums of squares |
| One running experiment per page | The variant is built from the control page as it is |
| An id is never reused | The analysis spends its error budget from the first look |

## Verification

| Test | Shows |
|------|-------|
| `adk-website/tests/s11_experiment.rs` | S11 end to end: the variant in a signed release, events through the real beacon route, daily looks, the lift significant before the breach and held back, the breach at look 6 (+25% sign-ups, 95% interval +2.1% to +47.9%; unsubscribes +100% against a 33% tolerance), the job paused, lowered and alarmed, the control republished, the Growth agent refused, a person's resume, the weekly review |
| `adk-website/tests/experiments.rs` | The charter mapping, plan validation, both record stores under one contract, builds that add and remove a variant, looks that add each whole day once, adoption without a pause, the kernel run, the client script under Node.js (`--run-ignored all`) |
| `adk-website/tests/experiments_property_tests.rs` | Stable assignment at its allocation; FNV-1a; every whole day counted once whatever the look schedule; clean add and remove across full and incremental builds; parsed log lines keep no query |
| `adk-website/tests/measurement.rs` | Each source through a real edge or the tracker: the beacon with Global Privacy Control, host logs in two formats, exactly-once orders counted once, verified agent calls, spend by vendor and agent, AEO citations by provider and rank |
| `adk-website/tests/live_experiment.rs` | On `claude-sonnet-5-5`, the paused Growth agent's call is refused and it reports the pause instead of acting |
