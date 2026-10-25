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
//! | [`alarms`] | P1, P2 and P3 routing to the owner's channels | §7.21 |
//! | [`review`] | The weekly review | §7.21 |
//! | [`wallet`] | The wallet as a KPI source and the site's commerce outcomes | §7.14 |
//! | [`workflows`] | The probe and the weekly review as kernel runs | §7.20 |
//! | [`observability`] | Spans, metrics and the site view | §7.24 |
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
pub mod incident;
pub mod jobs;
pub mod kinds;
pub mod observability;
pub mod onboarding;
pub mod packs;
pub mod playbooks;
pub mod review;
pub mod team;
pub mod themes;
pub mod wallet;
pub mod workflows;
