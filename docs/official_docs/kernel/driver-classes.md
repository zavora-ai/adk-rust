# Driver classes

A driver class fixes what a kind of service driver offers, so an agent's playbook does not
depend on which service the owner connected (spec 3.5 §7.13). `adk-drivers` defines seven
classes — DNS, registrar, email, analytics, search console, social and advertising — each
with its tools, their effect classes, the partition side each serves, and a default
autonomy cap. A class is served by a [kernel driver](drivers.md) whose manifest satisfies it,
or by an in-process adapter with the same tools.

> **Note:** `adk-drivers` is Experimental.

## Tools

| Class | Tool | Effect class | Side | Default cap |
|---|---|---|---|---|
| DNS | `dns_list_records` | `read` | back office | Read |
| | `dns_change_records` | `publish_public` | back office | Always held |
| Registrar | `registrar_domain_status` | `read` | back office | Read |
| | `registrar_renew_domain` | `financial` | back office | Within budget |
| Email | `email_list_inbox` | `read` | public | Read |
| | `email_reply_in_thread` | `send_external` | public | Within playbook |
| | `email_send_sequence_step`, `email_send_newsletter` | `send_external` | back office | Within playbook and consent |
| | `email_delivery_stats`, `email_check_domain` | `read` | back office | Read |
| Analytics | `analytics_metric` | `read` | back office | Read |
| Search console | `search_console_performance` | `read` | back office | Read |
| Social | `social_list_mentions`, `social_list_messages` | `read` | public | Read |
| | `social_reply_in_thread` | `send_external` | public | Propose |
| | `social_post` | `publish_public` | back office | Propose |
| Advertising | `ads_performance` | `read` | back office | Read |
| | `ads_launch_campaign` | `financial` (also publishes) | back office | Always held |
| | `ads_set_bid`, `ads_pause_campaign` | `financial` | back office | Within budget |

The public partition reads untrusted text and replies in its thread; everything that
publishes, changes DNS, spends or sends in bulk is in the back office. The public side is
partition `public`, the back office `back-office`.

## A manifest for any MCP server

`DriverClass::manifest_toml` writes the manifest of one side of a class, and
`DriverClass::check_manifest` and `DriverClass::check_listing` tell whether another
manifest, and the tools a server lists, satisfy the class:

```rust
use adk_drivers::{DriverKind, Side};
use adk_kernel::drivers::DriverManifest;

fn main() {
    let class = DriverKind::Email.class();
    let text = class.manifest_toml("email", "mcp-email", &["--imap"], Side::Public);
    let manifest = DriverManifest::from_toml(&text).expect("a manifest");
    assert!(class.check_manifest(&manifest, Side::Public).is_satisfied());
    println!("{text}");
}
```

A server that offers a tool beyond its side's contract fails `check_listing`: the kernel
would serve that tool to the partition classed `other`, so run the server with only that
side's tools.

## Autonomy caps

The kernel's driver policy already asks a person before every `publish_public` or
`financial` call of a kernel driver. `DriverCapPolicy` keeps the classes' caps for in-process
adapters too, and composes with the mode's policy through `StrictestPolicy`, never loosening
it:

| Cap | Decision |
|---|---|
| Always held — a DNS change, a campaign launch | Requires approval on every call, in every mode |
| Within budget, with an amount — a renewal's price, a bid | Allowed within the owner's `DriverBudget` for the tool, in its currency; requires approval beyond it |
| Any other | Decided by the other layers |

```rust
use std::sync::Arc;

use adk_core::{StrictestPolicy, ToolPolicy};
use adk_drivers::{DriverBudget, DriverCapPolicy};

fn website_policy(mode: Arc<dyn ToolPolicy>) -> Arc<dyn ToolPolicy> {
    let caps = DriverCapPolicy::new(
        DriverBudget::new()
            .cap("registrar_renew_domain", "USD", 30_00)
            .cap("ads_set_bid", "USD", 2_50),
    );
    Arc::new(StrictestPolicy::new(vec![mode, Arc::new(caps)]))
}
```

## Outbound rules

`EmailOutbox` and `SocialOutbox` keep the rules over any adapter:

| Rule | Spec |
|---|---|
| Every message and post ends with the `Disclosure` line: it names the owner and says an AI agent wrote it; a jurisdiction may reword it, never drop it | A-I17 |
| An unsubscribe or a revocation is read before every send; an unsubscribe mail, a bounce and a complaint are recorded as the inbox is read | A-I17 |
| A sequence step or newsletter goes only to a recipient with a live consent receipt for its scope | A-I8 |
| SPF, DKIM and DMARC are checked before the first send; sequences and newsletters wait until all three pass | §7.13 |
| `reply_in_thread` answers only a thread the inbox read, the sender only, from a playbook template, at a rate limit, never an automatic message | §7.10 |

## Reference adapters

| Class | Adapter | Feature |
|---|---|---|
| DNS | `Rfc2136Dns`: RFC 2136 dynamic update signed with TSIG, read by AXFR | `rfc2136` |
| Registrar | `RdapRegistrar`: expiry over RDAP; renewals go through the registrar's own driver | `rdap` |
| Email | `ImapSmtpEmail`: IMAP read-only, SMTP submission, TLS or STARTTLS (clear text on loopback only) | `imap-smtp` |
| Analytics | `PlausibleAnalytics`: the Plausible Stats API v2 | `plausible` |
| Search console | `SearchAnalyticsApi`: a Search Analytics `query` endpoint | `search-console` |
| Social | `MastodonSocial`: the Mastodon client API of an ActivityPub server | `mastodon` |
| Advertising | `RecordedAds`: recorded responses, so no test spends; `RecordingAds` records them | — |

Every class has an in-memory adapter, and the `conformance` feature carries each class's
suite for third-party adapters.

## KPI sources

Delivery statistics (`EmailOutbox`, source `email`), analytics (`AnalyticsMetrics`,
`analytics`), search performance (`SearchMetrics`, `search`) and ad spend (`AdsMetrics`,
`ads`) implement the kernel KPI tracker's `adk_kernel::charter::KpiSource`, so a KPI whose
`source` names one is measured from the driver. Each also lists its metrics (`metrics()`)
and measures one directly (`measure()`).

## Starting levels

`AutonomyCap::starting_level` gives the charter's `AutonomyLevel` a job using the tool
starts at: Observe for reads, Propose, "act within budget" for the playbook and budget caps,
and "act with approval" for a held tool, whose calls `DriverCapPolicy` holds on every rung.
