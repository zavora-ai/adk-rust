# website-scenarios

The demonstration scenarios of spec 3.5 (`docs/design/spec-3.5-agentic-web.md` §5, §11) as
product-level runs of `adk-website` against local fixtures. Each scenario asserts its pass
condition on stores, audit records, releases and the hosts' contents; agent turns run on
`ScriptedLlm` by default and on a real model with `--live`.

## Run

```bash
# Every scenario, scripted: no keys, no network
cargo run --manifest-path examples/website/Cargo.toml

# Some scenarios, with a report of every check and its evidence
cargo run --manifest-path examples/website/Cargo.toml -- S1 S5 --report scenarios.md

# Agent turns on claude-sonnet-5-5 (ANTHROPIC_API_KEY), or on gemini-3.7-flash (GOOGLE_API_KEY)
cargo run --manifest-path examples/website/Cargo.toml -- --live
cargo run --manifest-path examples/website/Cargo.toml -- --live --provider gemini

# Without the reference CDN host (S18), which needs `adk-site`'s object-store host
cargo run --manifest-path examples/website/Cargo.toml --no-default-features

# The S19 soak: 50,000 products over 24 simulated hours; writes its report
cargo run --release --manifest-path examples/website/Cargo.toml -- soak --report s19-soak.json

# Against the WordPress compose fixture (the nightly tier)
cargo run --manifest-path examples/website/Cargo.toml -- wordpress http://127.0.0.1:8080

# Re-record the inventories the pull-request tier runs against
cargo run --manifest-path examples/website/Cargo.toml -- record
```

Live runs read their keys from `WEBSITE_ENV_FILE`, else the repository's `.env`; see
`.env.example`. A scenario with no agent turn has nothing to run live: its scripted run is the
run.

## Scenarios

| # | Capability | Agent turns | Fixture |
|---|---|---|---|
| S1 | From nothing to live | The Steward's interview and the launch | The empty start |
| S2 | From an online presence | The Steward's interview | `fixtures/presence/` |
| S3 | Migrate and leave the infection behind | — | The recorded WordPress site with its `infected` overlay, replayed in isolation |
| S4 | Freeze in hours | — | The recorded WordPress site |
| S5 | Tamper, detect, reverse | The Guardian's rollback, held in Ask and alone in Auto | A launched site, its edge and the Guardian's probe |
| S6 | Identity, not claims | — | The edge with Web Bot Auth and an OAuth partner |
| S7 | Injection reaches nothing privileged | The Concierge on the edge and the Inbox | A hostile A2A message and a hostile email |
| S8 | Lead to warm handoff | The Concierge and the Revenue agent | A visitor, the contact form, the site's email outbox |
| S9 | Agent-to-agent business | — | A shop, a signed buyer agent, a sandbox rail |
| S10 | A week on duty | The Growth agent | Seven simulated days of questions and reviews, the weekly review |
| S11 | An experiment that respects its guardrails | — | The experiment generator, the edge's beacon, 30 simulated days |
| S12 | Money inside the box | The Finance agent | An owner-funded wallet with a mandate per category |
| S13 | Anyone can check | — | `awp verify` against a launched site and four tampering hosts |
| S14 | Earned autonomy | The Growth agent | 30 simulated days of review replies, then a guardrail breach |
| S15 | Agency mode | — | Three sites in one harness |
| S16 | A public body | — | A county site in English and Swahili, an OIDC identity provider |
| S17 | A personal site | — | A writer's WordPress export |
| S18 | Publishing at speed | The Steward and the Inbox | The owner's paired WhatsApp, an unpaired email, a newsroom on `DirectoryHost` and the reference CDN host |
| S19 | A site that never stops changing | — | A 600-product shop for four simulated hours (the soak: 50,000 for 24) |

A scenario prints `PASS`, `FAIL`, or `GAP` when the only checks that fail are ones a runtime
gap on `main` explains; the run lists every gap it found under "Runtime gaps".

## Fixtures

See [`fixtures/README.md`](fixtures/README.md).

| Fixture | What it is | Tier |
|---|---|---|
| `fixtures/wordpress/` | A compose file for a current, patched WordPress with seeded content, a contact-form plugin and a small shop, bound to loopback, and the `infected` overlay of inert markers | Nightly |
| `fixtures/recorded/` | The inventories each source takes of the WordPress fixture | Pull request |
| `fixtures/presence/` | Static listing, review and social pages of a business | Pull request |
| `fixtures/empty/` | The empty start | Pull request |

The recorded WordPress site itself — REST collections, crawled pages, uploads and the
`infected` overlay — is shared with `adk-site`'s migration tests under
`adk-site/tests/fixtures/migrate/`, so both run against one recording.

## CI

| Tier | Job | Runs |
|---|---|---|
| Pull request | `examples-compile` | The crate compiles |
| Pull request | `website-scenarios` | Every scenario, scripted, against the recordings |
| Nightly | `website-wordpress` | The compose fixture, clean and infected, against the recordings |
| Nightly | `website-soak` | S19 for 24 simulated hours, with its report as an artifact |
