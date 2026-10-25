//! # adk-drivers
//!
//! Service drivers for ADK-Rust agents (spec 3.5 §7.13): DNS and registrar, email,
//! analytics and search console, social and advertising.
//!
//! A driver is a kernel driver — a supervised MCP server under a manifest the owner
//! approved (spec 3.2 §7.7) — or an in-process adapter with the same tools. For each
//! class this crate gives:
//!
//! | Part | What it is |
//! |---|---|
//! | Contract | A trait for in-process adapters ([`dns::DnsDriver`], [`email::EmailDriver`], ...) and the class's tool list with effect classes, sides and default autonomy caps ([`DriverClass`]) |
//! | Manifest | [`DriverClass::manifest`] and [`DriverClass::manifest_toml`] make the kernel `DriverManifest` of one side; [`DriverClass::check_manifest`] and [`DriverClass::check_listing`] tell whether any MCP server and its manifest satisfy the class, so the owner connects whichever server they choose (D-13) |
//! | Rules | [`DriverCapPolicy`] holds DNS changes and campaign launches in every mode (A-I21) and within-budget calls beyond the owner's [`DriverBudget`]; the email and social outboxes keep the [`Disclosure`] line, unsubscribes, consent and `reply_in_thread`'s limits (§7.10, A-I8, A-I17) |
//! | Conformance | A suite per contract (`conformance`, feature `conformance`) |
//! | Adapters | In-memory adapters for hermetic tests, and a reference adapter per class over an open protocol or a common API |
//!
//! | Class | Tools | Reference adapter |
//! |---|---|---|
//! | DNS | `dns_list_records`, `dns_change_records` | [`dns::Rfc2136Dns`]: RFC 2136 with TSIG, AXFR |
//! | Registrar | `registrar_domain_status`, `registrar_renew_domain` | [`dns::RdapRegistrar`]: RDAP expiry |
//! | Email | `email_list_inbox`, `email_reply_in_thread`, `email_send_sequence_step`, `email_send_newsletter`, `email_delivery_stats`, `email_check_domain` | [`email::ImapSmtpEmail`]: IMAP and SMTP |
//! | Analytics | `analytics_metric` | [`analytics::PlausibleAnalytics`]: Plausible Stats API v2 |
//! | Search console | `search_console_performance` | [`analytics::SearchAnalyticsApi`]: Search Analytics API |
//! | Social | `social_list_mentions`, `social_list_messages`, `social_reply_in_thread`, `social_post` | [`social::MastodonSocial`]: Mastodon client API |
//! | Advertising | `ads_performance`, `ads_launch_campaign`, `ads_set_bid`, `ads_pause_campaign` | [`ads::RecordedAds`]: recorded responses, no live spend |
//!
//! `reply_in_thread` ([`ReplyInThread`]) is the public partition's one `SendExternal` tool
//! (§7.10, A-I2): it answers an email or social thread through its outbox.
//!
//! Delivery statistics, analytics, search performance and ad spend are [`KpiSource`]s
//! the KPI tracker reads (§7.11, §7.12).
//!
//! ## Features
//!
//! | Feature | Default | Adds |
//! |---|---|---|
//! | `rfc2136` | yes | [`dns::Rfc2136Dns`] and [`dns::tsig`] |
//! | `rdap` | yes | [`dns::RdapRegistrar`] |
//! | `imap-smtp` | yes | [`email::ImapSmtpEmail`] |
//! | `system-dns` | yes | [`email::SystemTxtLookup`] for SPF, DKIM and DMARC checks |
//! | `plausible` | yes | [`analytics::PlausibleAnalytics`] |
//! | `search-console` | yes | [`analytics::SearchAnalyticsApi`] |
//! | `mastodon` | yes | [`social::MastodonSocial`] |
//! | `conformance` | no | The conformance suites |
//!
//! ## Example
//!
//! ```rust
//! use std::sync::Arc;
//! use adk_core::{PolicyDecision, ToolPolicy, ToolPolicyRequest};
//! use adk_drivers::{DriverBudget, DriverCapPolicy, DriverKind, Side};
//! use adk_drivers::dns::{InMemoryDns, dns_tools};
//! use serde_json::json;
//!
//! # futures::executor::block_on(async {
//! // The DNS class's tools over an in-process adapter...
//! let tools = dns_tools(Arc::new(InMemoryDns::new().with_zone("bakery.example", [])));
//! assert_eq!(tools[1].name(), "dns_change_records");
//!
//! // ...or a manifest for any MCP server that serves them.
//! let manifest = DriverKind::Dns.class().manifest_toml("dns", "mcp-dns", &[], Side::BackOffice);
//! assert!(manifest.contains("dns_change_records = \"publish_public\""));
//!
//! // Either way, a change is held for the owner in every mode (A-I21).
//! let policy = DriverCapPolicy::new(DriverBudget::new());
//! let change = ToolPolicyRequest::new("dns_change_records", json!({"zone": "bakery.example"}));
//! assert!(matches!(policy.evaluate(&change).await, PolicyDecision::RequireApproval { .. }));
//! # });
//! ```

#![warn(missing_docs)]

pub mod ads;
pub mod analytics;
mod class;
#[cfg(feature = "conformance")]
pub mod conformance;
pub mod dns;
pub mod email;
mod error;
#[cfg(any(
    feature = "rdap",
    feature = "plausible",
    feature = "search-console",
    feature = "mastodon"
))]
mod http;
mod metrics;
mod outbound;
mod policy;
pub mod social;
mod tools;

pub use class::{
    AutonomyCap, BACK_OFFICE_PARTITION, DriverClass, DriverKind, Gap, ManifestCheck,
    PUBLIC_PARTITION, Side, ToolSpec, all_tools, tool_spec,
};
pub use error::{DriverError, Result};
pub use metrics::KpiSource;
pub use outbound::{
    ConsentCheck, Disclosure, IN_PROCESS_ORIGIN_PREFIX, InMemoryConsent, InMemorySuppressions,
    RateLimit, ReplyInThread, ReplyTemplates, Suppression, SuppressionList, SuppressionScope,
    ThreadReplier, recipient_key,
};
pub use policy::{BudgetCap, DRIVER_CAP_POLICY_REF, DriverBudget, DriverCapPolicy};
