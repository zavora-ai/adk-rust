//! The website tool catalogue: every tool's partition, effect, effect class and what may
//! allow it (spec 3.5 §7.10, §7.11, §7.13, A-I2, A-I11, A-I21).

use adk_core::ToolEffect;
use adk_kernel::drivers::EffectClass;

use crate::policy::{OriginClasses, ToolKind};

/// `Tool::origin` of the back-office tools: the harness classes them by this catalogue,
/// never by their own hints (spec 3.1 I12).
pub const BACK_OFFICE_ORIGIN: &str = "website:back-office";
/// `Tool::origin` of the public tools and the declared capabilities the public partition
/// serves (spec 3.5 §7.10).
pub const PUBLIC_ORIGIN: &str = "website:public";

/// Drafts or edits a content item on a draft branch (spec 3.5 §7.8.2 step 1).
pub const CONTENT_DRAFT: &str = "content_draft";
/// Edits a published item into a draft (spec 3.5 §7.8.2 step 1).
pub const CONTENT_EDIT: &str = "content_edit";
/// Schedules a draft's publication, embargo, expiry or reversal (spec 3.5 §7.8.2).
pub const CONTENT_SCHEDULE: &str = "content_schedule";
/// Publishes a correction with a visible note (spec 3.5 §7.8.2).
pub const CONTENT_CORRECT: &str = "content_correct";
/// Withdraws an item as a notice or 410 Gone (spec 3.5 §7.8.2).
pub const CONTENT_WITHDRAW: &str = "content_withdraw";
/// Reverts one item to an earlier signed version (spec 3.5 §7.8.3).
pub const CONTENT_REVERT: &str = "content_revert";
/// Drafts an item's translation (spec 3.5 §7.8.2 "Translations").
pub const TRANSLATION_DRAFT: &str = "translation_draft";
/// Takes an upload through the media pipeline (spec 3.5 §7.8.2 "Media").
pub const MEDIA_INTAKE: &str = "media_intake";
/// Builds a draft's release and publishes it to the host's preview (spec 3.5 §7.8.2 step 3).
pub const RELEASE_PREVIEW: &str = "release_preview";
/// Releases a draft to production (spec 3.5 §7.8.2 steps 4–8).
pub const SITE_PUBLISH: &str = "site_publish";
/// Rolls the whole site back to a signed release (spec 3.5 A-I11).
pub const SITE_ROLLBACK: &str = "site_rollback";
/// Reads what the host serves at a path (spec 3.5 §7.13 "Host").
pub const HOST_FETCH: &str = "host_fetch";
/// Runs the launch gate's full tier over the working copy (spec 3.5 §7.8.1).
pub const LAUNCH_GATE_RUN: &str = "launch_gate_run";
/// Runs `awp verify` against an origin with published-document requests only (spec 3.5
/// §7.7, A-I9).
pub const AWP_VERIFY: &str = "awp_verify";
/// Drafts the next charter version (spec 3.5 §7.6, §7.18 stage 5).
pub const CHARTER_DRAFT: &str = "charter_draft";
/// Asks every required signer to sign the drafted charter; commits nothing itself
/// (spec 3.5 §7.6, A-I23).
pub const CHARTER_COMMIT_REQUEST: &str = "charter_commit_request";
/// Asks the owner to promote a job; promotes nothing itself (spec 3.5 §7.11, A-I13).
pub const JOB_PROMOTION_REQUEST: &str = "job_promotion_request";
/// Reads a site, a profile or a listing and records what it finds as `Assumed` facts
/// (spec 3.5 §7.18 stage 2, A-I18).
pub const SITE_DISCOVER: &str = "site_discover";
/// Records an owner's interview answer: `Known` only from the owner's own words
/// (spec 3.5 §7.18 stage 4, A-I18).
pub const INTERVIEW_RECORD: &str = "interview_record";
/// The wallet's payment tool (`adk-payments`, spec 3.5 §7.14); it publishes itself, the
/// catalogue only knows its class.
pub const WALLET_PAY: &str = "wallet_pay";

/// Searches the current release's content (spec 3.5 §7.10).
pub const SEARCH_SITE: &str = "search_site";
/// Answers the person who wrote, in their thread (spec 3.5 §7.10).
pub const REPLY_IN_THREAD: &str = "reply_in_thread";
/// Sends the back office a typed signal (spec 3.5 §7.10).
pub const EMIT_SIGNAL: &str = "emit_signal";

/// `Tool::origin` of `adk-drivers`' `reply_in_thread`, the public partition's reply over the
/// email and social drivers' outboxes (spec 3.5 §7.10, §7.13).
pub const DRIVER_REPLY_ORIGIN: &str = "adk-drivers:reply";

/// Which partition a tool belongs to (spec 3.5 §7.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WebsitePartition {
    /// The agents that read untrusted text: Concierge, Agent desk, Inbox.
    Public,
    /// The agents that act for the owner: Steward, Growth, Revenue, Guardian, Finance.
    BackOffice,
}

/// What, beyond the job's autonomy level, decides a call (spec 3.5 §7.11, A-I13, A-I21).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Hold {
    /// The job's level alone.
    Ladder,
    /// The first production release — the launch — is held for the owner in every mode
    /// (A-I21); later ones follow the level.
    AtLaunch,
    /// Held for a person in every mode: a decision card.
    Always,
}

/// One website tool (spec 3.5 §7.10, §7.11).
///
/// # Example
///
/// ```rust
/// use adk_harness::website::{SITE_ROLLBACK, spec};
/// use adk_kernel::drivers::EffectClass;
///
/// let rollback = spec(SITE_ROLLBACK).unwrap();
/// assert_eq!(rollback.class, EffectClass::PublishPublic);
/// assert!(rollback.reversible, "a rollback to a signed release is pre-authorisable (A-I11)");
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolSpec {
    /// The tool's name.
    pub name: &'static str,
    /// Its partition.
    pub partition: WebsitePartition,
    /// What repeating a call does.
    pub effect: ToolEffect,
    /// What it does to the world, as a kernel driver manifest would class it.
    pub class: EffectClass,
    /// It only writes drafts and proposals, never the live site: allowed from Propose up.
    pub draft: bool,
    /// It is one of the acts "Act, reversible only" permits: drafts and a rollback to a
    /// signed release (A-I11).
    pub reversible: bool,
    /// What else decides it.
    pub hold: Hold,
}

const fn tool(
    name: &'static str,
    partition: WebsitePartition,
    effect: ToolEffect,
    class: EffectClass,
    draft: bool,
    reversible: bool,
    hold: Hold,
) -> ToolSpec {
    ToolSpec { name, partition, effect, class, draft, reversible, hold }
}

use EffectClass::{PublishPublic, Read, SendExternal, WriteLocal};
use ToolEffect::{Idempotent, NonIdempotent, ReadOnly};
use WebsitePartition::{BackOffice, Public};

/// The catalogue (spec 3.5 §7.10, §7.11, §7.13, §7.18).
///
/// | Tool | Partition | Effect | Class | Draft | Reversible | Hold |
/// |---|---|---|---|---|---|---|
/// | `content_draft`, `content_edit`, `translation_draft`, `media_intake`, `charter_draft` | back office | idempotent | `write_local` | yes | yes | level |
/// | `release_preview` | back office | idempotent | `write_local` | yes | yes | level |
/// | `site_discover`, `interview_record` | back office | idempotent | `write_local` | yes | yes | level |
/// | `content_schedule`, `content_correct`, `content_withdraw`, `content_revert` | back office | non-idempotent | `publish_public` | no | no | level |
/// | `site_publish` | back office | non-idempotent | `publish_public` | no | no | the launch in every mode (A-I21) |
/// | `site_rollback` | back office | non-idempotent | `publish_public` | no | yes (A-I11) | level |
/// | `host_fetch`, `launch_gate_run`, `awp_verify` | back office | read-only | `read` | — | — | — |
/// | `charter_commit_request`, `job_promotion_request` | back office | non-idempotent | `write_local` | no | no | every mode (a decision card) |
/// | `wallet_pay` | back office | non-idempotent | `financial` | no | no | level; the wallet holds over a cap |
/// | `search_site` | public | read-only | `read` | — | — | — |
/// | `reply_in_thread` (`adk-drivers`) | public | non-idempotent | `send_external` | no | no | the manifest policy; the driver keeps thread, template, rate and disclosure |
/// | `emit_signal` | public | non-idempotent | `write_local` | no | no | the manifest policy |
pub const CATALOG: &[ToolSpec] = &[
    tool(CONTENT_DRAFT, BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
    tool(CONTENT_EDIT, BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
    tool(TRANSLATION_DRAFT, BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
    tool(MEDIA_INTAKE, BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
    tool(CHARTER_DRAFT, BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
    tool(RELEASE_PREVIEW, BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
    tool(SITE_DISCOVER, BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
    tool(INTERVIEW_RECORD, BackOffice, Idempotent, WriteLocal, true, true, Hold::Ladder),
    tool(CONTENT_SCHEDULE, BackOffice, NonIdempotent, PublishPublic, false, false, Hold::Ladder),
    tool(CONTENT_CORRECT, BackOffice, NonIdempotent, PublishPublic, false, false, Hold::Ladder),
    tool(CONTENT_WITHDRAW, BackOffice, NonIdempotent, PublishPublic, false, false, Hold::Ladder),
    tool(CONTENT_REVERT, BackOffice, NonIdempotent, PublishPublic, false, false, Hold::Ladder),
    tool(SITE_PUBLISH, BackOffice, NonIdempotent, PublishPublic, false, false, Hold::AtLaunch),
    tool(SITE_ROLLBACK, BackOffice, NonIdempotent, PublishPublic, false, true, Hold::Ladder),
    tool(HOST_FETCH, BackOffice, ReadOnly, Read, false, false, Hold::Ladder),
    tool(LAUNCH_GATE_RUN, BackOffice, ReadOnly, Read, false, false, Hold::Ladder),
    tool(AWP_VERIFY, BackOffice, ReadOnly, Read, false, false, Hold::Ladder),
    tool(CHARTER_COMMIT_REQUEST, BackOffice, NonIdempotent, WriteLocal, false, false, Hold::Always),
    tool(JOB_PROMOTION_REQUEST, BackOffice, NonIdempotent, WriteLocal, false, false, Hold::Always),
    tool(WALLET_PAY, BackOffice, NonIdempotent, EffectClass::Financial, false, false, Hold::Ladder),
    tool(SEARCH_SITE, Public, ReadOnly, Read, false, false, Hold::Ladder),
    tool(REPLY_IN_THREAD, Public, NonIdempotent, SendExternal, false, false, Hold::Ladder),
    tool(EMIT_SIGNAL, Public, NonIdempotent, WriteLocal, false, false, Hold::Ladder),
];

/// The catalogue entry of `name`.
///
/// # Example
///
/// ```rust
/// use adk_harness::website::{SITE_PUBLISH, Hold, spec};
///
/// assert_eq!(spec(SITE_PUBLISH).unwrap().hold, Hold::AtLaunch);
/// assert!(spec("bash").is_none());
/// ```
pub fn spec(name: &str) -> Option<&'static ToolSpec> {
    CATALOG.iter().find(|spec| spec.name == name)
}

/// Classes the website tools by the catalogue (spec 3.5 §7.10, §7.11; spec 3.1 I12).
///
/// | Tool | Harness class | So the mode table |
/// |---|---|---|
/// | read-only | `External { ReadOnly, trusted }` | allows it in every mode |
/// | a draft or a decision card | `Todo` | allows it in every mode, as proposing is free; the ladder refuses it at Observe, and the website policy holds every decision card |
/// | `reply_in_thread` (`adk-drivers`), `emit_signal`, a declared capability | `Todo` | leaves it to the public partition's manifest policy, the one policy there (§7.10 "Mode") |
/// | any other back-office tool | `External { effect, trusted }` | denies it in read-only and plan, and asks in ask and auto unless an owner's standing rule allows it |
///
/// # Example
///
/// ```rust
/// use adk_core::ToolEffect;
/// use adk_harness::website::{BACK_OFFICE_ORIGIN, WebsiteClasses};
/// use adk_harness::{OriginClasses, ToolKind};
///
/// let classes = WebsiteClasses;
/// assert_eq!(classes.class_of(BACK_OFFICE_ORIGIN, "content_draft"), Some(ToolKind::Todo));
/// assert_eq!(
///     classes.class_of(BACK_OFFICE_ORIGIN, "site_publish"),
///     Some(ToolKind::External { effect: ToolEffect::NonIdempotent, trusted_origin: true })
/// );
/// assert_eq!(classes.class_of("mcp:fs", "content_draft"), None);
/// ```
#[derive(Clone, Copy, Debug, Default)]
pub struct WebsiteClasses;

impl OriginClasses for WebsiteClasses {
    fn class_of(&self, source: &str, tool: &str) -> Option<ToolKind> {
        let partition = match (source, tool) {
            (BACK_OFFICE_ORIGIN, _) => WebsitePartition::BackOffice,
            (PUBLIC_ORIGIN, _) | (DRIVER_REPLY_ORIGIN, REPLY_IN_THREAD) => WebsitePartition::Public,
            _ => return None,
        };
        let Some(spec) = spec(tool) else {
            // A declared capability the public partition serves: its manifest policy decides.
            return (partition == WebsitePartition::Public).then_some(ToolKind::Todo);
        };
        if spec.partition != partition {
            // A tool of the other partition under this origin is a lookalike, not ours.
            return None;
        }
        Some(match spec.effect {
            ToolEffect::ReadOnly => {
                ToolKind::External { effect: ToolEffect::ReadOnly, trusted_origin: true }
            }
            _ if spec.draft
                || spec.hold == Hold::Always
                || partition == WebsitePartition::Public =>
            {
                ToolKind::Todo
            }
            effect => ToolKind::External { effect, trusted_origin: true },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_only_drafts_and_rollback_are_reversible() {
        let names: std::collections::BTreeSet<&str> = CATALOG.iter().map(|s| s.name).collect();
        assert_eq!(names.len(), CATALOG.len());
        for spec in CATALOG {
            if spec.reversible {
                assert!(spec.draft || spec.name == SITE_ROLLBACK, "{} is reversible", spec.name);
            }
            if spec.draft {
                assert_ne!(spec.effect, ToolEffect::NonIdempotent, "{}", spec.name);
                assert_eq!(spec.class, EffectClass::WriteLocal, "{}", spec.name);
            }
            assert_eq!(spec.effect == ToolEffect::ReadOnly, spec.class == EffectClass::Read);
        }
    }

    #[test]
    fn only_publish_public_and_financial_tools_change_the_world() {
        for spec in CATALOG.iter().filter(|s| s.partition == WebsitePartition::BackOffice) {
            let world = matches!(spec.class, EffectClass::PublishPublic | EffectClass::Financial);
            assert_eq!(
                world,
                !spec.draft
                    && spec.effect == ToolEffect::NonIdempotent
                    && spec.hold != Hold::Always,
                "{}",
                spec.name
            );
        }
    }
}
