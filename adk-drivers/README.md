# adk-drivers

Service drivers for ADK-Rust agents (spec 3.5 §7.13,
[`docs/design/spec-3.5-agentic-web.md`](https://github.com/zavora-ai/adk-rust/blob/main/docs/design/spec-3.5-agentic-web.md)):
DNS and registrar, email, analytics and search console, social and advertising. Each class
is a contract with its tools, effect classes, partitions and default autonomy caps; the
kernel's driver manifest of those tools, so any MCP server that satisfies it is usable; a
conformance suite; and reference adapters.

[![Crates.io](https://img.shields.io/crates/v/adk-drivers.svg)](https://crates.io/crates/adk-drivers)
[![Documentation](https://docs.rs/adk-drivers/badge.svg)](https://docs.rs/adk-drivers)
[![License](https://img.shields.io/crates/l/adk-drivers.svg)](https://github.com/zavora-ai/adk-rust/blob/main/LICENSE)

**Stability: Experimental.**

## Classes

| Class | Tools (effect class) | Side | Default cap | Reference adapter |
|---|---|---|---|---|
| DNS | `dns_list_records` (read), `dns_change_records` (publish_public) | back office | Changes always held (A-I21) | `Rfc2136Dns`: RFC 2136 dynamic update with TSIG, read by AXFR |
| Registrar | `registrar_domain_status` (read), `registrar_renew_domain` (financial) | back office | Renewals within budget | `RdapRegistrar`: expiry over RDAP |
| Email | `email_list_inbox` (read), `email_reply_in_thread` (send_external) | public | Within playbook | `ImapSmtpEmail`: IMAP and SMTP |
| | `email_send_sequence_step`, `email_send_newsletter` (send_external), `email_delivery_stats`, `email_check_domain` (read) | back office | Within playbook and consent | |
| Analytics | `analytics_metric` (read) | back office | Read | `PlausibleAnalytics`: Plausible Stats API v2 |
| Search console | `search_console_performance` (read) | back office | Read | `SearchAnalyticsApi`: a Search Analytics `query` endpoint |
| Social | `social_list_mentions`, `social_list_messages` (read), `social_reply_in_thread` (send_external) | public | Propose | `MastodonSocial`: the Mastodon client API |
| | `social_post` (publish_public) | back office | Propose | |
| Advertising | `ads_performance` (read), `ads_launch_campaign` (financial, publish_public), `ads_set_bid`, `ads_pause_campaign` (financial) | back office | Launch always held; tuning within budget | `RecordedAds`: recorded responses, no live spend |

Every class also has an in-memory adapter for hermetic tests. Delivery statistics, analytics,
search performance and ad spend implement the kernel KPI tracker's `KpiSource`. `reply_in_thread`
(`ReplyInThread`) is the public partition's one `SendExternal` tool (§7.10): it answers an
email or social thread through its outbox.

## Rules

| Rule | Where | Spec |
|---|---|---|
| A DNS change and a campaign launch are held for the owner in every mode | `DriverCapPolicy` | A-I21, §7.13 |
| A renewal or a bid beyond the owner's budget is held | `DriverCapPolicy` with `DriverBudget` | §7.13 |
| Every outbound message ends with a line naming the owner and saying an AI agent wrote it | `Disclosure`, in both outboxes | A-I17 |
| An unsubscribe or a revocation takes effect before the next send | `SuppressionList`, read before every send | A-I17 |
| A sequence step or newsletter needs a live consent receipt, checked at send time | `ConsentCheck` | A-I8 |
| SPF, DKIM and DMARC are checked before the first send; bulk email waits until they pass | `DomainAuthCheck` in `EmailOutbox` | §7.13 |
| A reply goes to the thread's sender only, from a playbook template, at a rate limit, never to an automatic message | `EmailOutbox::reply`, `SocialOutbox::reply` | §7.10 |

## Quick start

```toml
[dependencies]
adk-drivers = "3.0.0"
```

```rust
use std::sync::Arc;

use adk_drivers::dns::{InMemoryDns, dns_tools};
use adk_drivers::{DriverBudget, DriverCapPolicy, DriverKind, Side};

// The DNS tools over an in-process adapter, for an agent's toolset.
let tools = dns_tools(Arc::new(InMemoryDns::new().with_zone("bakery.example", [])));

// Or the manifest of any MCP server that serves them, for `adk-rust drivers mount`.
let manifest = DriverKind::Dns.class().manifest_toml("dns", "mcp-dns", &[], Side::BackOffice);

// Composed with the mode's policy through `StrictestPolicy`, this holds every DNS change.
let policy = DriverCapPolicy::new(DriverBudget::new().cap("ads_set_bid", "USD", 250));
```

## Features

| Feature | Default | Adds |
|---|---|---|
| `rfc2136` | yes | `dns::Rfc2136Dns` and `dns::tsig` |
| `rdap` | yes | `dns::RdapRegistrar` |
| `imap-smtp` | yes | `email::ImapSmtpEmail` |
| `system-dns` | yes | `email::SystemTxtLookup` for SPF, DKIM and DMARC checks |
| `plausible` | yes | `analytics::PlausibleAnalytics` |
| `search-console` | yes | `analytics::SearchAnalyticsApi` |
| `mastodon` | yes | `social::MastodonSocial` |
| `conformance` | no | The conformance suites, for third-party adapters |

## Testing

Each suite runs against the in-memory adapters and the reference adapters: RFC 2136 against
an authoritative server on loopback (with BIND's `nsupdate` and `dig` checking its TSIG where
installed), IMAP and SMTP against loopback servers in plain text and under TLS, RDAP,
Plausible, the Search Analytics API and Mastodon against loopback servers answering with
recorded responses, and advertising against a recording.

```bash
cargo nextest run -p adk-drivers
```
