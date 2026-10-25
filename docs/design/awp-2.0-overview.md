# AWP 2.0 overview — websites that work for you

| | |
|---|---|
| **Status** | Draft for review, 2026-10-24 |
| **Audience** | Website owners and website developers; the maintainers of AWP, ADK-Rust and `adk-website` |
| **Layers** | **Protocol:** AWP 2.0 · **Runtime:** the ADK-Rust harness · **Product:** `adk-website`, a multi-agent proactive website |
| **Companion documents** | [Spec 3.5](spec-3.5-agentic-web.md): the full specification of all three layers in ADK-Rust; the normative AWP 2.0 text, published from it in the AWP repository |

---

## 1. Summary

A website today is a tool its owner has to operate. Someone patches it, feeds it content, promotes it, answers its email, watches its numbers and notices when it breaks. Most of the web still runs it on a server that executes code for every page request, and in October 2026 that model failed in public.

**AWP 2.0 changes what a website is.** An AWP website is a team of agents with a website: it builds itself, defends itself, serves people and AI agents through interfaces made for each, gets found by search engines, answer engines and agents, works towards the owner's goals around the clock, runs its own email, campaigns and budget within limits the owner sets, reports on its performance, and asks for help when it needs it.

> **Stop running a website. Hire one.**

![adk-website architecture: any site, visitors, static content, the agent edge, the kernel back office and the owner](images/spec-3.5/architecture-light.png)

AWP 2.0 has three layers, each with one home and one test:

| Layer | What it is | Home | Tested by |
|---|---|---|---|
| **Protocol — AWP 2.0** | The open contract between a website and the people and agents who use it | The AWP repository: normative text, JSON schemas, conformance kit | Anyone, against any site, with `awp verify` |
| **Runtime — the ADK-Rust harness** | The always-on, governed agent runtime that runs AWP websites | ADK-Rust | ADK-Rust's conformance suites and demonstration scenarios |
| **Product — `adk-website`** | A prebuilt, multi-agent, proactive website that a user spins up and that onboards its owner, builds or migrates itself, and deploys itself | The `adk-website` crate in ADK-Rust | The owner's KPIs |

- The protocol knows nothing about the harness; another runtime can implement AWP.
- The runtime knows nothing about any particular business.
- The product contains no framework code: a team definition, a charter, content, capability packs and configuration. When the product needs something the runtime lacks, it goes into the runtime.

## 2. Why websites must change now

| Shift | Evidence | Consequence |
|---|---|---|
| **The dynamic web became indefensible** | PHP2Shell (21 October 2026): unauthenticated remote code execution in every PHP installation serving web requests; updating the site or its plugins did not fix it; government services in several countries went offline. PHP runs about 69% of sites whose server language is known; WordPress runs about 40% of all sites. Two WordPress core flaws were mass-exploited within a day or two of their patches; the median time from disclosure to mass exploitation is five hours, and 46% of WordPress vulnerabilities had no fix at disclosure | A page request must not execute code. Patch-and-hope is over |
| **AI finds flaws faster than owners patch** | The 2026 WordPress and PHP flaws were found with AI assistance, one WordPress chain in about ten hours; agent-driven intrusion campaigns are documented | Defence has to be continuous and automatic, not a monthly chore |
| **Visitors are increasingly agents** | Agents search, compare, book and buy on people's behalf; identity, conversation, tool and payment standards for agents are converging (Web Bot Auth, A2A, MCP, AP2, ACP) | A site that cannot prove itself to agents, or let them act safely, loses those customers |
| **Websites are still passive** | Every builder and CMS waits for the owner to act; the 2026 vendor agents still need a person to approve each write, and none runs a site end to end | The value is in a site that does the work, within limits the owner controls |

AWP 1.0, drafted in April 2026, saw the agent shift early but mixed protocol, runtime and product in one set of documents, relied on claims (User-Agent strings, self-declared headers) instead of proof, and assumed a server composing every page. The runtime it needed did not exist yet. AWP 2.0 keeps 1.0's best ideas — dual audiences, owner sovereignty, proactivity, owner intelligence — and rebuilds the rest on a runtime that now exists.

## 3. What an AWP website is

| Property | What it means |
|---|---|
| **Proactive** | It works around the clock on the owner's goals and sleeps, at no cost, when there is nothing to do |
| **Self-defending** | No code runs when a page is requested; every release is signed; tampering is detected and reversed; it raises alarms through the owner's chosen channel |
| **Two interfaces** | People get fast, multilingual pages plus conversation and composed experiences; agents get a verified front door with typed capabilities |
| **Discoverable everywhere** | It optimises for search engines (SEO), answer engines (AEO) and agent search, and measures all three |
| **Goal-driven** | It holds a charter with goals, strategy and KPIs, and tracks performance over time |
| **A participant in its owner's work** | For a business, a shop, a charity, a newsroom or a public body alike, it captures, nurtures and hands off enquiries, sells, presents, experiments, keeps itself fresh, answers its own email and works its own campaigns |
| **Financially accountable** | It runs on an owner-funded wallet, pays its own running costs within mandates, and reports cost and return |
| **Accountable** | It reports on finances, marketing, engagement, traffic and trends, and asks the owner for input on a schedule |
| **Learning** | It keeps what works, fixes what fails, and asks for help when it cannot |
| **Self-building** | It builds its first release during onboarding and keeps building after launch |

## 4. The charter: how an AWP website is run

Every AWP website has a **charter**: its job description. The format is part of the AWP protocol, so a site's goals and authority move with it between hosts.

| Section | Holds | Set by |
|---|---|---|
| **Site kind and mission** | Personal, business, shop, public body, charity, school, newsroom, clinic, community or event; what it is for, who it serves, how it succeeds | Owner; the site proposes revisions each quarter |
| **Goals and KPIs** | For example leads per week, conversion, revenue, cost per lead, response time, freshness, uptime — each with a metric, source, baseline, target and horizon | Owner, from target ranges the site proposes with evidence |
| **Guardrail metrics** | Complaints, unsubscribes, refunds, spam reports, review sentiment | Owner; a breach pauses the job and overrides growth targets |
| **Authority** | The autonomy level of each job | Owner; autonomy is earned job by job |
| **Budget and wallet** | Spending caps per category: hosting, model tokens, advertising | Owner funds; the site accounts for every unit |
| **Voice and red lines** | Brand voice, claims the site may make, topics it avoids | Owner |
| **Reporting and escalation** | Weekly review, urgency levels, channels per urgency | Owner |

The site runs one loop continuously: **charter → plan → work → measure → report → learn**, and the learning feeds the next charter revision.

### 4.1 Earned autonomy

| Level | The site |
|---|---|
| **Observe** | Reports only |
| **Propose** | Drafts the work and waits for a decision |
| **Act with approval** | Acts after each approval |
| **Act within budget** | Acts alone inside limits |
| **Act, reversible only** | Acts alone where the action can be undone |

Every job starts at Observe or Propose. When the owner approves a job's proposals unchanged for an agreed run, the weekly review offers to promote that job; the owner accepts or declines, and can demote any job at any time. Trust comes from a record, not from a settings page.

## 5. The product: `adk-website`

`adk-website` is a prebuilt AWP website. A user spins it up, and the site takes it from there: it onboards its owner, builds a new site or migrates an existing one, passes its own launch gate, and deploys itself.

### 5.0 Every kind of website

PHP runs about 70% of the sites whose server language is known, and most of them are not shops. `adk-website` serves all of them: the first fact of every charter is the **site kind**, and the kind's template decides the goals, agents, capabilities and rules.

| Kind | What the site works towards | What it adds |
|---|---|---|
| **Personal** — blogs, portfolios, CVs | Readers, subscribers, enquiries | Newsletter with consent; smallest budget; no sales or advertising |
| **Business** — services, local firms, professionals | Qualified enquiries, bookings, reviews | Leads, nurture and warm handoff |
| **E-commerce** — shops, WooCommerce stores | Orders, conversion, refunds, stock accuracy | Catalogue, orders, payments, product feeds for agents |
| **Government and public sector** | Services completed, accessibility, accurate notices | Two-person publication; WCAG as a hard gate; every official language; no trackers or advertising; service forms with verified identity |
| **Nonprofit and charity** | Donations, volunteers, sign-ups | Donations with receipts, volunteer and event sign-ups |
| **Education** | Admissions enquiries, open days | Enquiries and bookings; safeguarding red lines |
| **News and media** | Readers, subscribers, corrections time | Subscriptions, licensing, AI usage terms; never generated news |
| **Health** | Appointments, response time | Bookings; no clinical advice; health data handled as a special category |
| **Community and faith** | Attendance, members reached, donations | Events and donations on a volunteer's budget |
| **Events** | Registrations, tickets, questions answered | Registration and tickets; archives itself after the event |

Organisations get roles and approval chains instead of a single owner, so a ministry can require an editor and a publisher for every release. Forums, learning platforms and member portals migrate their public pages; their account areas are a named decision until a later spec covers them.

### 5.1 Spinning it up

| Route | For | What happens |
|---|---|---|
| **One command** | Developers | A single command scaffolds and starts `adk-website` locally and opens onboarding in the browser |
| **Deploy button** | Owners | A host's one-click deploy starts the same image in onboarding mode |
| **Agency console** | Developers running sites for clients | One harness hosts many sites, each its own partition; the developer prepares onboarding and the client signs the charter |

The first page the owner sees is the site's own Steward agent, asking for a URL, a name, or nothing at all.

### 5.2 Onboarding: the site builds itself while the owner talks

| # | Stage | The site | The owner | Result |
|---|---|---|---|---|
| 1 | **Start** | Asks for a URL, a name, or nothing; and a phone number for urgent contact | Answers | No installation steps |
| 2 | **Discover** | Crawls any existing site, checks its risk and agent readiness, reads public listings, reviews, competitors, search presence and how answer engines describe the business | Watches, or leaves and comes back | A business snapshot with a source behind every fact, marked unconfirmed |
| 3 | **Protect** | For a site on a legacy runtime, serves a crawled snapshot and takes the old server off the public path (freeze) | Approves one decision | Value before anything is asked |
| 4 | **Interview** | Asks only what it could not find; uses choices and short forms; each answer updates a live preview within seconds | Answers by chat, voice or WhatsApp, in short resumable sessions | Owner-stated facts with provenance |
| 5 | **Charter** | Turns plain-language goals into KPIs with baselines and target ranges; proposes guardrails, conservative authority, a budget with a cost estimate, red lines and escalation | Reviews each section as a decision card; edits; signs | Charter version 1, signed |
| 6 | **Build** | Builds release 1 from the charter: structure where every page has a job and a KPI, content from confirmed facts only, design from existing brand assets or three directions, every chosen language, capabilities, the agent and search layer, legal pages, privacy-respecting measurement, and only the agents the business needs | Picks a design direction; answers the questions the build raises | A signed preview release |
| 7 | **Launch gate** | Runs fixed checks: accessibility, performance budget, links, no unconfirmed claims, legal pages, signed release, `awp verify`; for a migration, URL coverage and visual and text parity with the old site | Reviews the preview page by page | A release ready to publish |
| 8 | **Connect and deploy** | Guides each connection — domain, email, channels, analytics, payment — through OAuth, a credential broker or device pairing; deploys the static site, the agent edge and the Steward | Grants each scoped connection; never types a password into chat; approves going live | A live site and a small starting wallet |
| 9 | **Run** | Plans 30, 60 and 90 days; works in Propose mode for the first week; locks baselines on day 0; sends the first weekly review on day 7 with results, cost, questions and jobs ready for promotion | Approves, edits and promotes | The routine loop |

| Starting point | Built from |
|---|---|
| **Existing site** (WordPress, PHP, legacy application) | The crawl and export, cleaned of infections; every URL keeps working |
| **Online presence, no site** | Business profiles, social pages, listings, reviews, and documents or photos the owner shares |
| **Nothing** | The interview and the charter |

### 5.3 The team

Each agent is a harness agent; the Steward leads the team. Agents in the **public** partition read untrusted text and can answer within a conversation and emit typed signals, but cannot spend, publish, send in bulk or read the owner's memory. Agents in the **back office** act only on typed signals and the charter.

| Agent | Partition | Jobs |
|---|---|---|
| **Concierge** | Public | Conversation with visitors in their language, presentations and walkthroughs, answers from the site's own content, lead capture |
| **Agent desk** | Public | The front door for verified agents over A2A, MCP and HTTP: quotes, bookings, held actions |
| **Inbox** | Public | The site's email, social mentions and messages: triage and replies within playbooks |
| **Steward** | Back office | Onboarding, charter, planning, KPIs, weekly review, owner liaison, autonomy promotion |
| **Growth** | Back office | SEO, AEO and agent search; freshness; languages; experiments; advertising; social publishing |
| **Revenue** | Back office | Lead scoring, nurture sequences with consent, warm handoff, orders and payments |
| **Guardian** | Back office | Signed releases, drift detection and rollback, site watch, alarms, incident mode |
| **Finance** | Back office | Wallet, cost per lead and per sale, profit and loss, financial reports, budget requests |

### 5.4 Jobs

| Area | Job | Owner agent | Starts at |
|---|---|---|---|
| **Run and protect** | Defend: static content, signed releases, drift rollback, site watch, abuse throttling | Guardian | Act, reversible only |
| | Raise alarms by urgency and channel | Guardian | Act |
| | Stay fresh: update stale facts, fix links, retire outdated pages | Growth | Within budget for edits; Propose for new pages |
| | Replace the hosting control panel: hosting, domains, certificates, backups | Guardian, Finance | Propose; renewals within budget |
| **Be found** | SEO: technical SEO, structured data, sitemaps, speed | Growth | Within budget |
| | AEO: answer-ready content, FAQ markup, citations; track how answer engines describe the site | Growth | Within budget; tracking at Observe |
| | Agent search: signed contract, agent cards, capability manifests; track agent discovery and use | Growth | Act |
| **Serve people and agents** | Multilingual pages and conversation | Concierge, Growth | Act; a new language at Propose |
| | Presentations tailored to role and need | Concierge | Act |
| | Agent front door: ask, quote, book, buy under policy | Agent desk | Act within policy |
| | The site's own email | Inbox | Within playbook; Propose otherwise |
| | Social engagement that brings people to the site | Inbox, Growth | Propose, then playbook |
| | Advertising campaigns | Growth, Finance | Propose to launch; within budget to tune |
| **Grow** (by kind: enquiries, orders, donations, subscribers, services) | Capture leads, sign-ups and requests from every channel | Concierge, Inbox, Agent desk | Act |
| | Qualify and nurture with consent | Revenue | Within playbook |
| | Warm handoff to a person with full context | Revenue | Act |
| | Sell: catalogue, quotes, checkout, bookings, signed receipts | Revenue | Within payment policy |
| | Experiment and optimise with statistical evidence | Growth | Within budget |
| | Learn: playbooks from what works, fixes or help requests for what fails | Steward | Propose |
| **Run the money** | Pay its own hosting, model tokens and advertising under mandates | Finance | Within budget |
| | Cost and return by channel; move budget towards what pays | Finance | Within budget |
| **Be accountable** | Goals, strategy and KPIs over time | Steward | Propose |
| | Weekly review: finances, marketing, engagement, traffic, trends | Steward, Finance | Act |
| | Batched questions for the owner | Steward | Act |

### 5.5 Publishing

Every change becomes a small signed release through one pipeline: draft, check, preview with page diffs, approve, release, publish, announce, measure. Only the changed pages and what depends on them are rebuilt, so a single change is live within about a minute of approval.

| Case | How it arrives | Who approves | What else happens |
|---|---|---|---|
| **New product** | A message with photos from the owner's phone, the web editor, or an import | The owner, until the job earns autonomy | Description only from supplied facts; category pages, product feed and machine views rebuilt; scheduled launch; agents notified |
| **Price change** | Owner, import or feed | Within the charter's price rule on its own; larger changes wait | A price is always a signed claim; open quotes keep their price |
| **News story** | The reporter | Editor, then publisher, from their phones | The site helps with headlines, translations and SEO but never writes news; embargoes; visible corrections with a provable history |
| **Blog post** | The writer, or notes the site turns into a draft | The writer | Scheduled; newsletter to subscribers who consented |
| **Website update** | "Closed Monday for the holiday" | Owner, or a public body's approval chain | Every place the fact appears is updated, and the change reverts itself on Tuesday |

Stock and availability are served live by the edge rather than rebuilt with every sale. Content can be authored only from the owner's paired devices, people with a role in the web editor, the content repository and configured feeds — never from the public inbox.

**Constantly updated sites** — a shop syncing 50,000 products, a newsroom on election night, a public body in an emergency — stay static and signed. Every update goes into one of three lanes:

| Lane | Carries | How it reaches visitors |
|---|---|---|
| **Release** | Anything an agent may act on: articles, product pages, prices, notices, final results | Signed releases, batched every 10–60 seconds |
| **Live** | State: stock, availability, live scores | Read-only answers from the edge, refreshed in seconds; no rebuilds |
| **Stream** | Live blogs, tickers, emergency updates | An edge feed that appears in seconds and is folded into a signed release every few minutes |

Feed updates run without a model, so a busy site does not spend tokens on stock counts. A broken or compromised feed that tries to change too much at once is held whole until the owner decides. A bad edit is undone for that item alone, without losing every other change.

### 5.6 For developers and agencies

| WordPress today | `adk-website` |
|---|---|
| Plugins run inside the site with full access, and are where most compromises start | **Capability packs**: skills, capabilities and drivers that are sandboxed, declare their effects and are signed |
| Themes | Themes and design systems that the build step uses |
| Agencies sell builds and maintenance | Agencies sell migrations, capability packs and charters, and run many sites from one harness |
| Security plugins and scanners | `awp verify` reports any site's risk and agent readiness; it is also the agency's first conversation with a prospect |

## 6. The runtime: the ADK-Rust harness

An AWP website needs a runtime that is always on but asleep when idle, that governs every action, that keeps public input away from privileged tools, that can hold an action for a person's decision for as long as it takes, and that records everything. The ADK-Rust harness provides most of that today.

| Need | In ADK-Rust today | New for AWP 2.0 |
|---|---|---|
| Always-on agents that sleep until woken | `adk-kernel`: processes, agenda, wakes on time, file, webhook, queue and approval, quiet hours, proactivity policy (spec 3.2) | Site process kinds; a signal wake |
| Every action governed | One tool path: policy, guardrails, confirmation, budgets, action ledger, audit; kill switch | — |
| Public input kept from privileged tools | Kernel partitions with cross-partition access denied by default | The public and back-office partition definitions; typed signals |
| Decisions from anywhere | Decision cards, inbox, device pairing, chat channel contract, voice (spec 3.3) | Channel adapters (ported from `adk-gateway`) |
| Facts with provenance; memory | Long-term facts, `Known` only by owner confirmation (spec 3.1) | — |
| Money under mandates | `adk-payments` (ACP, AP2), payment mandates, spend ledger | Wallet funding; use-based payment rail for tokens and APIs |
| Statistical experiments | `adk-eval` baselines and paired A/B comparison | KPI tracker over time |
| Tamper-evident records | Signed, hash-chained audit log with signed notes | Signed releases and the site log |
| Agent protocols | A2A v1.0 server and client | MCP server; Web Bot Auth verifier and signer |
| Learning | Spec 3.4 specifies the self-improvement loop (not yet built) | Its application to playbooks and site configuration |
| The site itself | — | Site pipeline: sources, sanitiser, generator, launch gate, releases, hosts; the agent edge; charter and KPI stores; host, DNS, email, analytics, social and advertising drivers; the `site` preset and onboarding |

**Deployment shape.** An `adk-website` runs as three parts: the **content** on any static host, the **agent edge** as one small Rust service on its own origin, and the **Steward and back office** as a kernel service. If the edge and the kernel both stop, the site keeps serving its last signed release.

## 7. The protocol: AWP 2.0

AWP 2.0 defines only what a stranger can observe and test. It adopts the standards that settled in 2026 and adds what they lack.

| Adopts | For |
|---|---|
| Web Bot Auth (HTTP Message Signatures) | Requester identity |
| A2A v1.0 | Agent conversation |
| MCP | Tools |
| IETF AI preferences | Content usage terms |
| AP2, ACP, UCP and use-based payment rails | Payments |
| schema.org, `security.txt` | Structured data, disclosure |

| Adds | What it is |
|---|---|
| **Signed site contract** | One signed document at `/.well-known/awp.json`: identity, keys, profile, policies, links to every agent surface |
| **Static profile** | Pages, machine twins and manifests as plain files; no code runs per request |
| **Signed releases** | Every published state as a signed list of file digests in an append-only log |
| **Key hierarchy** | Root key offline and anchored in DNS; short-lived delegated keys; rotation and recovery |
| **Site status** | Operational, degraded, read-only, maintenance or incident, signed |
| **Trust tiers from proof** | Public, known, verified, partner — earned by verified identity and owner allowlists |
| **Typed capabilities** | Schemas, effect, minimum trust, price, consent scope, approval rule; one definition exposed over A2A, MCP and HTTP |
| **Held actions** | A pending response with a status link while the owner decides |
| **Exactly-once actions** | Idempotency keys on every one-time action |
| **Events** | Signed, durable notifications with a published catalogue and replay |
| **Consent receipts** | Signed, per scope and policy version, with revocation, export and erasure |
| **Embeddable components** | Script-free fragments an agent can show its own user |
| **Charter format** | Goals, KPIs, guardrails, authority, budget and voice; the public parts published, the rest private |
| **Conformance** | An executable conformance kit and `awp verify` |

**Dropped from the 1.0 draft:** server-composed pages on every request (now optional components on a static page); reverse-proxy pass-through (now freeze mode); User-Agent and `Accept` headers as trust (now hints); DIDs as the only route to verified trust; AWP's own agent message envelope and in-process bus (now A2A and a partition boundary); symmetric payment signatures (now signed receipts any agent can verify); the `awp.dev` namespace, which an unrelated company owns; pricing, agent layout, owner interface design and certification registries, which belong to products.

## 8. Why it is safe to let a website act

| Risk | Control |
|---|---|
| A runtime flaw on the page path | No code runs per page request |
| Host tampering or defacement | Signed releases, drift detection, rollback |
| Prompt injection through visitors, email, social or agents | Public agents hold no privileged tools; the back office receives typed signals only |
| Impersonated agents | Verified identity; claims are ignored |
| Runaway spending | Owner-funded wallet, mandates, caps per category, anomaly alarms; the site never opens accounts, adds payment methods or raises its own limits |
| Growth at any cost | Guardrail metrics override targets; no dark patterns; claims only from confirmed facts |
| Unlawful or dishonest messaging | Disclosure as the site owner's AI agent; consent and anti-spam rules; new claims and campaigns need approval |
| Credential theft during setup | Connections by OAuth, broker or pairing; the site never asks for a password |
| Irreversible mistakes | Everything is a signed release or a recorded action; rewind; kill switch; incident autonomy limited to rollback and read-only |

What remains attack surface, stated plainly: the build pipeline, the host and registrar accounts, DNS, and the edge and kernel services. AWP watches and protects them; it never claims a site is unhackable.

## 9. Adoption

| Step | Owner sees | Time |
|---|---|---|
| **Freeze** | The old site keeps working, served as a snapshot; the old server goes private | Hours |
| **Create or migrate** | A new site built from the owner's goals, or the old one cleaned and rebuilt with every URL intact | Days |
| **Open the front door** | Verified agents start asking, quoting, booking and buying | After launch |
| **Hand over the routine** | The site runs itself; the owner approves from the phone and reads one weekly review | Ongoing |

## 10. Roadmap

| Phase | Protocol | Runtime | Product |
|---|---|---|---|
| **1. Leave the runtime** | Contract, static profile, signed releases, keys, status, charter format, conformance kit and `awp verify` | Site pipeline, launch gate, hosts, site log, charter store, `site` preset | Spin-up, onboarding with self-build, migration and freeze, deployment; Steward and Guardian; weekly review |
| **2. Open the front door** | Verified identity, trust tiers, typed capabilities, held actions, exactly-once, events, consent | Agent edge, MCP server, Web Bot Auth, durable edge stores, partitions and signals | Concierge and Agent desk; multilingual conversation; leads and handoff |
| **3. Work full time** | — | KPI tracker, autonomy promotion, email and analytics drivers | Growth (SEO, AEO, agent search, freshness, experiments), Inbox, the full weekly review |
| **4. Run the money** | Commerce and receipts | Wallet and payment rails, social and advertising drivers, self-improvement (spec 3.4) applied to playbooks | Revenue and Finance; campaigns; profit and loss |

## 11. Decisions and open questions

**Decided**

| | Decision |
|---|---|
| D1 | Protocol is AWP 2.0; runtime is the ADK-Rust harness; product is `adk-website`, a multi-agent proactive website |
| D2 | Static content, dynamic edge: no code runs per page request |
| D3 | Public and back-office partitions, joined only by typed signals |
| D4 | The charter is part of the protocol; autonomy is earned per job |
| D5 | Onboarding is agent-led and includes the site building itself, from an existing site, an online presence, or nothing |
| D6 | `adk-website` is a crate in ADK-Rust, run with `adk-rust website` or its own binary; it holds no framework code |
| D7 | Every kind of site, one product: the site kind selects goals, agents, capabilities and rules; organisations get roles and approval chains |

**Open**

| # | Question | Recommendation |
|---|---|---|
| Q1 | The protocol's home: `agenticwebprotocol.com` does not resolve, `awp.dev` belongs to someone else, and `/.well-known/awp.json` collides with an expired IETF draft | Publish the AWP 2.0 text at a domain the project controls and register the well-known URI with IANA |
| Q2 | Naming beside the runtime's site crate | `adk-site` for the pipeline library and `adk-website` for the product; `agenticwebprotocol.com` as the first `adk-website` instance |
| Q3 | The first release's job set | Phase 1 and 2 jobs; advertising, social and the wallet last, because they spend money and speak in public |
| Q4 | Wallet rails | A use-based rail for model tokens and APIs, AP2 mandates for purchases; cloud and advertising cards stay with the owner until a record exists |
| Q5 | `adk-gateway` | Port its channel adapters to the kernel's channel contract, then retire it |
| Q6 | Where the always-on part runs by default | Any small container or server, a managed agent runtime, or the owner's own machine through `adk-rust up --install`; no default host |

## References

- PHP security advisories and releases, 20–23 October 2026: <https://www.php.net/archive/2026.php>
- BleepingComputer, PHP2Shell emergency patches: <https://bleepingcomputer.com/news/security/php-ships-emergency-patches-for-critical-php2shell-flaws>
- Vercel bulletin, php2shell: <https://vercel.com/kb/bulletin/php2shell-security-bulletin>
- W3Techs, PHP and WordPress usage (2026-10-24): <https://w3techs.com/technologies/details/pl-php>, <https://w3techs.com/technologies/details/cm-wordpress>
- Patchstack, State of WordPress Security in 2026: <https://patchstack.com/whitepaper/state-of-wordpress-security-in-2026/>
- Wiz, wp2shell: <https://wiz.io/blog/wp2shell-cve-2026-63030-cve-2026-60137>
- Web Bot Auth working group: <https://datatracker.ietf.org/wg/webbotauth/documents/>
- A2A specification: <https://a2a-protocol.org/latest/specification/>
- MCP Server Cards, SEP-2127: <https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2127>
- IETF AI preferences: <https://datatracker.ietf.org/wg/aipref/documents/>
- AP2 at the FIDO Alliance: <https://blog.google/products-and-platforms/platforms/google-pay/agent-payments-protocol-fido-alliance/>
- Cloudflare, agent readiness: <https://blog.cloudflare.com/agent-readiness>
