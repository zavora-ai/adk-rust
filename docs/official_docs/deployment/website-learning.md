# Playbook Learning

Each job of an AWP website runs from a playbook, a skill file in its agent's workspace. Once a
week the `playbook-learning` job reads the outcomes — experiment decisions and KPIs against the
owner's targets — and proposes revisions to the playbooks behind them. A revision lands only as
the owner's decision (spec 3.5 §7.15, A-I13). `adk-website`'s `learning` module implements it,
and a served site runs it with no configuration.

> **Note:** `adk-website` is Experimental. The learning job calls no model: each proposal is
> drafted from the outcome's numbers.

## When a proposal is drafted

The learning run fires on Mondays at 07:30 UTC (`learning::LEARNING_CRON`), after the weekly
review, as the kernel workflow `website.steward.learning`. It reviews the week before.

| Outcome | Enough evidence | Playbook | The change |
|---------|-----------------|----------|------------|
| An experiment | The sequential test stopped it with a decision under its fixed error budget: the variant adopted, or the control kept because the variant is worse, with the primary metric ready | The playbook of the experiment's job | A line to prefer, or to avoid, what the experiment tested, with the measured rates |
| A KPI | A baseline and the owner's target (A-I14), and the means of two weekly reviews in a row short of the target in the KPI's direction | The playbook `learning::kpi_playbook` names for the KPI's metric | A line to put the work that moves the KPI first in each run |

These propose nothing:

- an experiment that is running, or that a guardrail or its sample limit stopped;
- a KPI in its measure-only period, without a target, or short of its target for one week only;
- an outcome that proposed before: an experiment once, a KPI at most once in 28 days.

| KPI metric | Playbook |
|------------|----------|
| `leads.*`, `events.*` | `capture` |
| `newsletter.*`, `subscriptions.*`, `donations.*`, `volunteers.*` | `nurture` |
| `orders.*`, `payments.*`, `tickets.*`, `bookings.*`, `services.*` | `sell` |
| `content.*`, `catalog.*`, `notices.*`, `corrections.*` | `freshness` |
| `visits.*`, `search.*` | `seo` |
| `pages.*` | `aeo` |
| `agents.*` | `agent-search` |
| `conversations.*`, `a11y.*` | `presentations` |
| `reviews.*` | `review-replies` |
| `email.*` | `email` |
| `spend.*`, `profit`, `revenue` | `cost-and-return` |
| Anything else, or a playbook no agent of the site runs | `goals` |

## What a proposal holds

| Member | Holds |
|--------|-------|
| `playbook` | The playbook to revise, and `baseVersion`, the version it revises |
| `diff` | The change as a unified diff of the playbook's text: one line added under `## Learned` |
| `evidence` | Each outcome in one line, with a link to the weekly review that reports it on the web home (`/surface/website/review?week=…#experiments` or `#kpis`) |
| `expectedEffect` | What the change is expected to do |
| `state` | `reported`, `proposed`, `accepted` or `declined` |

Proposals are kept in `data/learning/proposals.json`.

## The owner's decision

| The learning job's level | A proposal |
|--------------------------|------------|
| Observe | Is reported only: no decision card, no change |
| Propose, and every level above it | Is the owner's decision card (`website_playbook_revision`) in the kernel's approval inbox |

| The owner | Result |
|-----------|--------|
| Accepts | The proposed text is the playbook's next version |
| Edits | The `text` the owner wrote is the next version |
| Declines, or the card expires | Nothing changes |

No level applies a revision on its own: a promoted learning job still proposes, and only the
owner's answer on the card changes a playbook. Each answer counts on the ladder for the
learning job as an unchanged approval, an edit or a rejection, so two declines in a row lower
it to Observe (§7.11).

## Versions

`learning::PlaybookVersions` keeps each playbook's versions:

| Version | Text |
|---------|------|
| 1 | The playbook in the agents' workspaces: a developer's, else the built-in one |
| 2 and later | Each accepted revision, kept in `data/playbooks/{playbook}.json` with the proposal and the person who accepted it |

An accepted version is written to `data/agents/{agent}/.skills/{playbook}.md` for every agent
that runs the playbook, where the harness discovers the agent's skills. When another
revision of the same playbook was accepted first, an unedited proposal's line goes onto the
newer text.

## Related

- [The AWP Website Product](adk-website.md): the jobs, the ladder and the weekly review
- [Website Experiments and Measurement](website-experiments.md): experiments and the KPI sources the outcomes come from
- [The Charter](charter.md): KPIs, baselines and the owner's targets
