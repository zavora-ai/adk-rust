//! # adk-website
//!
//! A prebuilt AWP 2.0 website (spec 3.5 Part C, §7.16–§7.24): a site that onboards its
//! owner, builds or migrates itself, passes its own launch gate, and then works towards the
//! owner's goals with a team of eight agents, inside the authority, budget and guardrails of
//! its charter.
//!
//! The product holds no framework code (D-1): the harness supplies the agents, tools,
//! partitions and the autonomy ladder; the kernel the charter store, KPI tracker, signals,
//! pairing and processes; `adk-site` the pipeline, gate, releases and probes; `adk-payments`
//! the wallet. This crate decides what a website *is* for each kind of site.
//!
//! | Module | Contents | Spec 3.5 |
//! |---|---|---|
//! | [`kinds`] | One charter template per site kind, the rules each adds, kind detection | §7.16.1, D-15 |
//! | [`team`] | The eight agents; the back office as a `TeamSpec` led by the Steward; building them over the harness kits | §7.19, §7.10 |
//! | [`jobs`] | The §7.20 jobs, their ladder bindings, first-week authority, and the kernel processes that run them | §7.20, §7.11 |
//! | [`onboarding`] | The nine-stage plan, three starting points, the setup-code gate, discovery, building release 1, the live preview, the cost estimate | §7.17, §7.18, A-I20 |
//! | [`themes`] | Three themes with light and dark tokens for the built-in renderer | §7.16, §7.23 |
//! | [`packs`] | Capability declarations: contact, quote, booking, order, search, components | §7.16, §7.4 |
//! | [`playbooks`] | A skill file for every job | §7.16, §7.15 |
//! | [`incident`] | Drift, incident status, rollback by standing rule or proposal, the person's exit | §7.20, A-I11 |
//! | [`inbox`] | The Inbox: the site's email and the chat messages of unpaired accounts, answered in their own thread | §7.19, §7.10, X12, A-I24 |
//! | [`leads`] | Lead scores and the handoff threshold, consent for nurture, the durable contact book, nurture sends inside quiet hours | §7.20, §7.21, S8, A-I8 |
//! | [`alarms`] | P1, P2 and P3 routing to the owner's channels | §7.21 |
//! | [`channel`] | The owner's chat channel: photos into the Steward's draft, alt text confirmed from the thread | §7.8.2, S18, A-I24 |
//! | [`repository`] | Files and git as an entry point: a signed push from an authorised committer proposes a release | §7.8.2, A-I24 |
//! | [`publish_delay`] | The delay a page the site drafts on its own waits before it can go live | §7.20, S10 |
//! | [`quotes`] | The reviews discovery found, the owner's approvals to quote them, and the launch gate's check over them | §7.18, S2 |
//! | [`review`] | The weekly review | §7.21 |
//! | [`learning`] | Outcomes into playbook revision proposals, decided by the owner; versioned playbooks | §7.15, A-I13 |
//! | [`upkeep`] | Upgrade proposals for advisories against the edge's lockfile, certificate renewal proposals | §7.20, S10 |
//! | [`wallet`] | The wallet as a KPI source and the site's commerce outcomes | §7.14 |
//! | [`experiments`] | Charter KPIs and guardrails as experiments, variants as static files, daily looks, pauses on a breach, the review's evidence | §7.12, A-I15 |
//! | [`measurement`] | KPI sources that are not drivers: the beacon, host logs, the commerce, action and spend ledgers, agent search at the edge, AEO | §7.12, §7.11 |
//! | [`workflows`] | The probe and the weekly review as kernel runs | §7.20 |
//! | [`contract`] | The signed contract, key set and charter terms released with the site | §7.2, §7.7, D-11 |
//! | [`deploy`] | The four deployment shapes, agency mode and serving the kernel and the edge | §7.22, A-I22 |
//! | [`edge`] | The site on the agent edge: the packs' handlers, the adapters to the back office, the read-only step | §7.9, §7.16, A-I11 |
//! | [`observability`] | Spans, metrics and the site view | §7.24 |
//! | [`ui`] | Onboarding and owner views on the web home: setup code, onboarding, charter cards, the site view, the weekly review, the web editor | §7.16, §7.18, §7.21, §7.24, §8.2 |
//!
//! # Example
//!
//! ```rust
//! use adk_harness::website::SiteRef;
//! use adk_website::kinds::{CharterInputs, template};
//! use adk_website::team::{AgentRole, SiteTeam};
//! use awp_types::SiteKind;
//!
//! let id = uuid::Uuid::new_v4();
//! let personal = template(SiteKind::Personal);
//! let team = SiteTeam::new(SiteRef::new(id, "notes", "https://notes.example"), &personal);
//! assert!(team.runs(AgentRole::Guardian) && !team.runs(AgentRole::Revenue));
//!
//! let inputs = CharterInputs::new(id, "Notes", "user:wanjiru", "Essays on cities.", vec!["en".parse().unwrap()], "USD");
//! let charter = personal.draft_charter(&team, &inputs);
//! assert!(charter.validate().is_ok());
//! ```

#![warn(missing_docs)]

pub mod alarms;
pub mod audit;
pub mod channel;
pub mod contract;
pub mod deploy;
pub mod edge;
pub mod experiments;
pub mod inbox;
pub mod incident;
pub mod jobs;
pub mod kinds;
pub mod leads;
pub mod learning;
pub mod measurement;
pub mod observability;
pub mod onboarding;
pub mod packs;
pub mod playbooks;
pub mod publish_delay;
pub mod quotes;
pub mod repository;
pub mod review;
pub mod team;
pub mod themes;
pub mod ui;
pub mod upkeep;
pub mod wallet;
pub mod workflows;
