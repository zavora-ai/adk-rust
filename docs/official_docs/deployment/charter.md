# The charter

The charter is what a site is for and what it may do: its kind, who governs it, its mission,
goals and KPIs, the guardrails that override growth, each job's authority, the budget, the
voice, the red lines and how it reports (spec 3.5 §7.6, D-6 to D-9). Its format is part of AWP
2.0, `awp_types::Charter`, so a site's goals, history and authority move with it to any
conforming runtime. The public part is published; the rest stays in the charter store.

> **Note:** the charter types are in `awp-types`, which is **Beta**. The charter store, the KPI
> tracker and the autonomy ladder are `adk-kernel`'s `charter` feature
> ([Charter, KPIs, Signals and Earned Autonomy](../kernel/charter.md)).

| Who uses it | How |
|-------------|-----|
| Onboarding | Drafts version 1 from the site kind's template; the owners review each section and sign |
| The kernel | Keeps every committed version, measures each KPI against its baseline, pauses a job whose guardrail breaks, holds each job to its level |
| The harness | Compiles each job's level into the policy its agent runs under |
| The edge | Publishes the public terms and asks for the consent scopes they name |
| Onboarding and the weekly review | Check the running-cost estimate against the budget, and report the month's spend against it |

## Sections

| Member | Type | Holds |
|--------|------|-------|
| `version` | `u32` | Set by the charter store when a version is committed |
| `siteId` | UUID | The site |
| `kind` | `SiteKind` | `personal`, `business`, `ecommerce`, `government`, `nonprofit`, `education`, `media`, `health`, `community` or `events` |
| `governance` | `Governance` | The people who hold each role, and the approval chain per action class |
| `mission`, `strategy` | text, `StrategyPillar`s | What the site is for and how it gets there |
| `goals` | `Goal`s | Each goal names the KPIs that measure it |
| `kpis` | `Kpi`s | Metric, source, direction, baseline, target, cadence |
| `guardrails` | `Guardrail`s | Metric, threshold, breach direction, window, action, jobs |
| `authority` | `JobAuthority`s | Each job's autonomy level and limits |
| `budget` | `Budget` | Period, currency and caps per spending category |
| `voice` | `Voice` | Tone, languages and the disclosure line |
| `redLines` | `RedLine`s | Claims, topics and actions never allowed |
| `reporting` | `Reporting` | Review cadence, the channels per urgency, quiet hours |
| `public` | `PublicTerms` | Usage terms, policies, consent scopes and contact: the published part |
| `priceRule` | `PriceRule` | Price changes that act without the owner (default within 10%) |
| `signed` | `OwnerSignature` | The decision, the owner, the signed note and the time; absent on a draft |

## An example

A bakery's charter, trimmed from the business kind's template. Durations are seconds and
amounts are minor units of the budget's currency.

```json
{
  "version": 1,
  "siteId": "0b2f7c1e-5d4a-4c3b-9a8e-1f2d3c4b5a69",
  "kind": "business",
  "governance": {
    "roles": [
      { "principal": "amina", "roles": ["owner", "editor"] },
      { "principal": "baraka", "roles": ["publisher"] }
    ],
    "approvalChains": [
      { "action": "charter_change", "roles": ["owner"] },
      { "action": "publish", "roles": ["editor", "publisher"] }
    ]
  },
  "mission": "Fresh rye and sourdough for Nakuru, baked every morning.",
  "strategy": [{ "name": "Win qualified enquiries" }, { "name": "Earn good reviews" }],
  "goals": [
    { "id": "enquiries", "name": "Win qualified enquiries", "kpis": ["qualified_leads"] },
    { "id": "reputation", "name": "Earn good reviews", "kpis": ["review_rating"] }
  ],
  "kpis": [
    {
      "id": "qualified_leads",
      "name": "Qualified enquiries",
      "metric": "leads.qualified",
      "source": "commerce",
      "direction": "up",
      "baseline": { "value": 12.0, "window": 1209600, "measuredAt": "2026-10-12T00:00:00Z" },
      "target": { "value": 20.0, "horizon": "2027-01-31T00:00:00Z" },
      "cadence": "daily"
    },
    {
      "id": "review_rating",
      "name": "Average review rating",
      "metric": "reviews.rating",
      "source": "reviews",
      "direction": "up",
      "cadence": "daily"
    }
  ],
  "guardrails": [
    {
      "id": "unsubscribes",
      "metric": "email.unsubscribe_rate",
      "threshold": 0.02,
      "breach": "above",
      "window": 604800,
      "action": "pause_job",
      "jobs": ["email", "nurture"]
    }
  ],
  "authority": [
    { "job": "onboarding", "level": "act_with_approval" },
    { "job": "protect", "level": "propose" },
    { "job": "seo", "level": "propose", "limits": { "actionsPerDay": 100 } },
    { "job": "running-costs", "level": "propose", "limits": { "spendCap": 5000, "actionsPerDay": 100 } }
  ],
  "budget": {
    "period": "monthly",
    "currency": "KES",
    "caps": { "hosting": 200000, "models": 500000, "advertising": 0, "domains": 30000, "other": 100000 }
  },
  "voice": {
    "tone": "Warm, clear and practical",
    "languages": ["en", "sw"],
    "disclosure": "Written by Juniper Bakery's assistant, an AI agent, on behalf of the site's owner."
  },
  "redLines": [
    { "kind": "action", "text": "Never invent facts, testimonials, team members, prices or credentials" },
    { "kind": "claim", "text": "No health claims about bread" }
  ],
  "reporting": {
    "review": "weekly",
    "routes": [
      { "urgency": "p1", "channels": ["whatsapp", "sms"] },
      { "urgency": "p2", "channels": ["whatsapp"] }
    ],
    "quietHours": { "start": "21:00:00", "end": "07:00:00", "timezone": "Africa/Nairobi" }
  },
  "public": {
    "usage": "train-ai=n, search=y",
    "policies": [
      { "name": "privacy", "url": "/privacy/", "version": "1" },
      { "name": "terms", "url": "/terms/", "version": "1" }
    ],
    "consentScopes": ["contact", "newsletter"],
    "contact": { "security": "/.well-known/security.txt", "page": "/contact/" }
  },
  "priceRule": { "autoWithinPct": 10.0 }
}
```

`adk-website new` writes a draft like this for the kind you choose, `charter.draft.json`, and
`adk-rust awp migrate-config` drafts one from an AWP 1.x `business.toml`. The JSON Schema is
`awp-types/schemas/2.0/charter.schema.json` (feature `json-schema`).

## Site kinds

The kind is the charter's first fact. `adk_website::kinds::template(kind)` holds each kind's
goals, KPIs, guardrails, jobs, agents, capabilities and rules, and
`KindTemplate::draft_charter` turns them into version 1 with the owner holding the owner and
editor roles — and the publisher role, unless the kind requires two-person publication. A
government site's template, for example, adds an editor-and-publisher chain for `publish`, every
official language, WCAG 2.2 AA and no third-party scripts ([Site kinds](adk-website.md#site-kinds)).
The owner can change anything a template proposes.

## Governance

| Role | Typical holder |
|------|----------------|
| `owner` | The person or people the site answers to; at least one is required |
| `publisher` | Approves what goes live |
| `editor` | Proposes and edits content |
| `approver` | Approves an action class a chain names |
| `auditor` | Reads the record |

An approval chain lists the roles an action class needs: `publish`, `charter_change`, or any
other class a product names. A chain is satisfied only by distinct people, each holding the
role they approve in; the proposer of an action cannot also approve it, and no agent holds a role
— a principal under `urn:adk:agent:` is refused (A-I23). A charter with no `charter_change` chain
needs one owner to commit a new version; with one, a draft cannot loosen the chain that approves
it, because the chain in force decides.

## KPIs and baselines

| Member | Values |
|--------|--------|
| `metric` | A metric name, such as `leads.qualified` or `pages.answer_citations` |
| `source` | `beacon`, `host_logs`, `analytics:<driver>`, `commerce`, `spend`, `email`, or a name a product registers |
| `direction` | `up` or `down` |
| `baseline` | `{value, window, measuredAt}` |
| `target` | `{value, horizon}`; only beside a baseline |
| `cadence` | `daily`, `weekly`, `monthly` or `quarterly` |

No KPI has a target until it has a baseline, and the owner sets targets (A-I14), so the site
is never graded against numbers it chose. The KPI tracker
measures a KPI without a baseline for 14 days, locks the period's mean as its baseline, and
reports it to the owner; the target comes with the next charter version. The measurement sources
are the edge's beacon, host logs, analytics and search-console drivers, the commerce and spend
ledgers and the email driver ([Website experiments and measurement](website-experiments.md#measurement-sources)).

## Guardrails

A guardrail names a metric, a `threshold`, whether a breach is `above` or `below` it, a
`window` over which the metric's mean is taken, an `action`, and the `jobs` it governs.
Guardrails override growth targets (A-I15):

| Action | On a breach |
|--------|-------------|
| `pause_job` | Each job is paused before its next action, the owner gets a P2 alarm, and each job is lowered one autonomy level |
| `alert` | The P2 alarm and the demotion, without a pause |

A breach acts once until the metric recovers. Only a person resumes a paused job. An experiment
monitors the guardrails of its job throughout and stops when one breaks
([Website experiments and measurement](website-experiments.md)).

## Authority

Each job runs at a level the charter sets, with optional `limits`: `spendCap` bounds the amount
of the job's financial calls, and `actionsPerDay` caps the tool calls of a run.

| Level | The job's agent |
|-------|-----------------|
| `observe` | Reads only; output goes to reports |
| `propose` | Drafts and proposals; nothing acts |
| `act_with_approval` | Every non-read call is held for the owner |
| `act_reversible_only` | Acts alone only for reversible acts: a rollback to a signed release, the edge in read-only mode, drafts |
| `act_within_budget` | Acts within its rules and budget; anything beyond is held |

A new site starts its jobs at `observe` or `propose`; onboarding acts with approval. The site may
lower a job's level — on a guardrail breach, two rejections in a row, or an incident in its
area — and never raise one: raising a level is an owner's decision that creates the standing
rules the level needs (A-I13). After 20 unchanged approvals within 30 days the weekly review
offers a promotion. The harness compiles each level into policy
([The website runtime](../harness/website.md#the-autonomy-ladder)).

## Budget, voice and red lines

| Section | Members |
|---------|---------|
| `budget` | `period` (a cadence), `currency`, and `caps` per category: `hosting`, `models`, `advertising`, `domains`, `other`. Payments run under the wallet's own category mandates, which hold a payment over a cap and which the site never raises (A-I16; [Wallet](../security/wallet.md)) |
| `voice` | `tone`, `languages` (the first is the default), and `disclosure`: the line every outbound message carries to say an AI agent wrote it (A-I17) |
| `redLines` | `kind` (`claim`, `topic` or `action`) and `text`; a launch gate given them (`LaunchGate::with_red_lines`) refuses an item that states one |
| `priceRule` | `autoWithinPct`: a price change within it acts within budget; a larger one is held for the owner, and an open quote keeps its price until it expires (A-I25) |

## Reporting

`review` sets the review cadence. `routes` maps each urgency to the owner's channels — `web`,
`chat`, `email`, `sms`, `voice`, `whatsapp`, `telegram`, `slack`, `discord`, `matrix` —
and `quietHours` gives a start, an end and an IANA time zone.

| Urgency | Delivery |
|---------|----------|
| `p1` | At once on the P1 channels, inside quiet hours too, repeated until acknowledged |
| `p2` | On the owner's chosen channel, outside quiet hours |
| `p3` | The weekly review |

## Public terms

`public` is the only part of the charter a visitor or an agent sees. It is published at
`/.well-known/awp/charter.json`, and its digest is in the contract's `charter.digest`, so
`adk-rust awp verify` fails with `charter.digest` when the two disagree.

| Member | Published as |
|--------|--------------|
| `usage` | The `Content-Usage` line of `robots.txt` (IETF AI preferences) |
| `policies` | Name, URL, version and optional digest of each policy page |
| `consentScopes` | The consent scopes the site asks for: `session_continuity`, `proactive_outreach`, `persistent_profile`, `payment_data`, `newsletter`, or a scope a capability names, such as `contact` |
| `contact` | The security contact, an email and a contact page |

## Validation

`Charter::validate()` returns every violation at once:

| Violation | Rule |
|-----------|------|
| `TargetWithoutBaseline` | A target needs a baseline (A-I14) |
| `NoOwner` | At least one person holds the owner role |
| `AgentHoldsRole` | Roles belong to people (A-I23) |
| `EmptyApprovalChain`, `DuplicateApprovalChain` | A chain names roles, and each action class has at most one |
| `UnsatisfiableApprovalChain` | Enough distinct people hold the chain's roles |
| `DuplicateKpi`, `UnknownKpi` | KPI ids are unique, and goals name only defined KPIs |

```rust
use awp_types::{Charter, CharterViolation};

fn check(charter: &Charter) {
    if let Err(error) = charter.validate() {
        for violation in &error.violations {
            match violation {
                CharterViolation::TargetWithoutBaseline { kpi } => println!("measure {kpi} first"),
                other => println!("{other}"),
            }
        }
    }
}
```

## Signing and committing

A charter version takes effect only when its owners commit it. `CharterStore::commit` validates
the draft, refuses an agent among the role holders, checks the decision against the
`charter_change` chain in force, gives the version the next number, and stores it with an
`OwnerSignature`: the decision's id, the first owner who approved, the signed note that seals
the commit in the audit log, and the time. The store is append-only, so the site's charter
history has no gaps ([The charter store](../kernel/charter.md#the-charter-store)).

| Surface | How the owners sign |
|---------|---------------------|
| Onboarding on the web home | Each section as a card; **Sign as** for each role the person holds ([The Website's Views](../surfaces/website.md)) |
| The harness | `charter_commit_request` is a decision card that commits nothing; `SiteRuntime::sign_charter` commits once every required signer has signed |
| Code | `CharterStore::commit(draft, decision)`, or `commit_audited` to record the chain and the commit in the audit log |

`adk-rust site` reads `charter.json`, or the draft `charter.draft.json`, from the project: a
charter whose `publish` chain needs two or more roles makes the `site` commands that change the
site refuse, because such a site publishes only through its approvals, each role by a different
person
([Sites and AWP from the command line](site-cli.md#the-site-project)).

## Related

- [Charter, KPIs, Signals and Earned Autonomy](../kernel/charter.md) — the store, the tracker, the ladder and signals
- [The AWP website product](adk-website.md) — site kinds and the team
- [Website experiments and measurement](website-experiments.md)
- [Agentic Web Protocol (AWP) 2.0](awp.md)
