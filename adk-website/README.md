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
| `packs` | Capability declarations: contact, quote, booking, order, search, components | §7.16, §7.4 |
| `playbooks` | A skill file for every job, installed into each agent's workspace | §7.16, §7.15 |
| `incident` | Drift, incident status, rollback to the last good release by standing rule or proposal, a person's exit | §7.20, A-I11 |
| `alarms` | P1, P2 and P3 routing to the owner's channels | §7.21 |
| `review` | The weekly review | §7.21 |
| `wallet` | The wallet as a KPI source and the site's commerce outcomes | §7.14 |
| `workflows` | The probe and the weekly review as kernel runs | §7.20 |
| `observability` | Spans, metrics and the site view | §7.24 |

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

Serving the site, the agent edge and the kernel from the binary arrives with the deployment
shapes (spec 3.5 §7.22).

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

See [the docs page](../docs/official_docs/deployment/adk-website.md).
