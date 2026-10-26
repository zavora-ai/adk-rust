//! The words the owner's views use (spec 3.3 §8, spec 3.5 §7.24): plain language, sentence
//! case, no internal identifiers.

use adk_site::gate::{CheckStatus, GateCheck};
use adk_site::pipeline::PageChange;
use awp_types::{AutonomyLevel, ContentKind, ContentState, Role, SiteKind, SiteState};

use super::{Pill, Tone};

/// How much a job does alone, in the words of spec 3.3 §8.1 ("Just look · Suggest · Ask
/// first · Go ahead"), for the five rungs of the ladder (spec 3.5 §7.11).
pub(crate) fn level(level: AutonomyLevel) -> &'static str {
    match level {
        AutonomyLevel::Observe => "Just look",
        AutonomyLevel::Propose => "Suggest",
        AutonomyLevel::ActWithApproval => "Ask first",
        AutonomyLevel::ActWithinBudget => "Go ahead within budget",
        AutonomyLevel::ActReversibleOnly => "Only what can be undone",
        _ => "Another level",
    }
}

/// The site's status as a pill (spec 3.5 §7.5, §7.24).
pub(crate) fn site_state(state: SiteState) -> Pill {
    match state {
        SiteState::Operational => Pill::new("Up", Tone::Good),
        SiteState::Degraded => Pill::new("Partly working", Tone::Waiting),
        SiteState::ReadOnly => Pill::new("Read only", Tone::Waiting),
        SiteState::Maintenance => Pill::new("Maintenance", Tone::Info),
        SiteState::Incident => Pill::new("Incident", Tone::Bad),
        _ => Pill::new("Unknown", Tone::Neutral),
    }
}

/// A launch-gate check in the owner's words (spec 3.5 §7.8.1).
pub(crate) fn check(check: GateCheck) -> &'static str {
    match check {
        GateCheck::ServerCode => "No code runs when a page is requested",
        GateCheck::Claims => "Every claim is one you confirmed",
        GateCheck::Links => "No broken links",
        GateCheck::Accessibility => "Accessible to everyone (WCAG 2.2 AA)",
        GateCheck::Performance => "Fast to load",
        GateCheck::Legal => "Privacy, terms and consent pages",
        GateCheck::MachineViews => "A version of every page for agents",
        GateCheck::MigrationParity => "Matches the old site",
        GateCheck::Quarantine => "Nothing quarantined is published",
        GateCheck::RequiredLanguages => "Every language the site needs",
        GateCheck::RedLines => "Nothing on your never list",
        GateCheck::StructuredData => "Search engines can read it",
        GateCheck::ThirdPartyResources => "Nothing loads from other sites",
        GateCheck::Signing => "Signed with the key you delegated",
        _ => "Another check",
    }
}

/// How a check ended, as a pill.
pub(crate) fn check_status(status: CheckStatus) -> Pill {
    match status {
        CheckStatus::Passed => Pill::new("Passed", Tone::Good),
        CheckStatus::Failed => Pill::new("Needs a fix", Tone::Bad),
        CheckStatus::NotMeasured => Pill::new("Not measured here", Tone::Neutral),
        CheckStatus::Skipped => Pill::new("Not checked yet", Tone::Neutral),
    }
}

/// How a page changes in a preview (spec 3.3 §7.5 "shown before they happen").
pub(crate) fn page_change(change: PageChange) -> Pill {
    match change {
        PageChange::Added => Pill::new("New page", Tone::Good),
        PageChange::Changed => Pill::new("Changed", Tone::Info),
        PageChange::Removed => Pill::new("Removed", Tone::Bad),
    }
}

/// Where a content item stands (spec 3.5 §7.8.2).
pub(crate) fn content_state(state: ContentState) -> Pill {
    match state {
        ContentState::Draft => Pill::new("Draft", Tone::Neutral),
        ContentState::InReview => Pill::new("Waiting for approval", Tone::Waiting),
        ContentState::Approved => Pill::new("Approved", Tone::Info),
        ContentState::Scheduled => Pill::new("Scheduled", Tone::Info),
        ContentState::Published => Pill::new("Live", Tone::Good),
        ContentState::Corrected => Pill::new("Live, corrected", Tone::Good),
        ContentState::Withdrawn => Pill::new("Withdrawn", Tone::Neutral),
        _ => Pill::new("Saved", Tone::Neutral),
    }
}

/// A content kind's name (spec 3.5 §7.8.2).
pub(crate) fn content_kind(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Page => "Page",
        ContentKind::Post => "Blog post",
        ContentKind::Article => "News article",
        ContentKind::Product => "Product",
        ContentKind::Event => "Event",
        ContentKind::Notice => "Notice",
        ContentKind::Service => "Service",
        ContentKind::Person => "Person",
        ContentKind::Collection => "Collection",
        _ => "Item",
    }
}

/// A role's name (spec 3.5 §7.6).
pub(crate) fn role(role: Role) -> &'static str {
    match role {
        Role::Owner => "Owner",
        Role::Publisher => "Publisher",
        Role::Editor => "Editor",
        Role::Approver => "Approver",
        Role::Auditor => "Auditor",
        _ => "Role",
    }
}

/// The wire name of a role, as forms send it back.
pub(crate) fn role_id(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Publisher => "publisher",
        Role::Editor => "editor",
        Role::Approver => "approver",
        Role::Auditor => "auditor",
        _ => "role",
    }
}

/// A role from its wire name.
pub(crate) fn role_from(id: &str) -> Option<Role> {
    [Role::Owner, Role::Publisher, Role::Editor, Role::Approver, Role::Auditor]
        .into_iter()
        .find(|role| role_id(*role) == id)
}

/// The wire name of a site kind.
pub(crate) fn kind_id(kind: SiteKind) -> &'static str {
    match kind {
        SiteKind::Personal => "personal",
        SiteKind::Business => "business",
        SiteKind::Ecommerce => "ecommerce",
        SiteKind::Government => "government",
        SiteKind::Nonprofit => "nonprofit",
        SiteKind::Education => "education",
        SiteKind::Media => "media",
        SiteKind::Health => "health",
        SiteKind::Community => "community",
        SiteKind::Events => "events",
        _ => "other",
    }
}

/// Every site kind, in the order of spec 3.5 §7.16.1.
pub(crate) const KINDS: [SiteKind; 10] = [
    SiteKind::Personal,
    SiteKind::Business,
    SiteKind::Ecommerce,
    SiteKind::Government,
    SiteKind::Nonprofit,
    SiteKind::Education,
    SiteKind::Media,
    SiteKind::Health,
    SiteKind::Community,
    SiteKind::Events,
];

/// A job area's name (spec 3.5 §7.20).
pub(crate) fn area(area: &str) -> &'static str {
    match area {
        "run-and-protect" => "Run and protect",
        "be-found" => "Be found",
        "serve" => "Serve",
        "grow" => "Grow",
        "money" => "Money",
        "account" => "Account",
        _ => "Other",
    }
}

/// An agent's name on screen (spec 3.5 §7.19).
pub(crate) fn agent(role: crate::team::AgentRole) -> &'static str {
    use crate::team::AgentRole;
    match role {
        AgentRole::Concierge => "Concierge",
        AgentRole::AgentDesk => "Agent desk",
        AgentRole::Inbox => "Inbox",
        AgentRole::Steward => "Steward",
        AgentRole::Growth => "Growth",
        AgentRole::Revenue => "Revenue",
        AgentRole::Guardian => "Guardian",
        AgentRole::Finance => "Finance",
    }
}

/// A spend category's name.
pub(crate) fn category(category: awp_types::SpendCategory) -> &'static str {
    use awp_types::SpendCategory;
    match category {
        SpendCategory::Hosting => "Hosting",
        SpendCategory::Models => "Models",
        SpendCategory::Advertising => "Advertising",
        SpendCategory::Domains => "Domains",
        SpendCategory::Other => "Other",
        _ => "Other",
    }
}

/// An amount in minor units of `currency`, in major units with the currency code:
/// `KES 1,250.00`.
pub(crate) fn money(minor: u64, currency: &str) -> String {
    let exponent = minor_exponent(currency);
    let major = minor as f64 / 10f64.powi(exponent);
    format!("{} {}", currency.to_ascii_uppercase(), grouped(major, exponent))
}

/// An amount already in major units: `KES 1,250.00`.
pub(crate) fn major(amount: f64, currency: &str) -> String {
    format!("{} {}", currency.to_ascii_uppercase(), grouped(amount, minor_exponent(currency)))
}

fn grouped(value: f64, decimals: i32) -> String {
    let text = format!("{:.*}", decimals.max(0) as usize, value.abs());
    let (whole, fraction) =
        text.split_once('.').map_or((text.as_str(), None), |(w, f)| (w, Some(f)));
    let mut out = String::new();
    for (index, digit) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    if let Some(fraction) = fraction {
        out.push('.');
        out.push_str(fraction);
    }
    if value < 0.0 { format!("-{out}") } else { out }
}

/// The ISO 4217 minor-unit exponent of `currency`.
pub(crate) fn minor_exponent(currency: &str) -> i32 {
    const ZERO: &[&str] = &[
        "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX", "VND", "VUV",
        "XAF", "XOF", "XPF",
    ];
    const THREE: &[&str] = &["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"];
    let currency = currency.to_ascii_uppercase();
    if ZERO.contains(&currency.as_str()) {
        0
    } else if THREE.contains(&currency.as_str()) {
        3
    } else {
        2
    }
}

/// A number for a KPI line: whole numbers without decimals, others with up to two.
pub(crate) fn number(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 {
        grouped(value, 0)
    } else if value.abs() < 1.0 {
        format!("{value:.2}")
    } else {
        grouped(value, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_is_grouped_in_major_units() {
        assert_eq!(money(125_000, "kes"), "KES 1,250.00");
        assert_eq!(money(1_500, "JPY"), "JPY 1,500");
        assert_eq!(major(12.5, "USD"), "USD 12.50");
        assert_eq!(number(1234.0), "1,234");
        assert_eq!(number(0.034), "0.03");
    }

    #[test]
    fn roles_round_trip_through_their_wire_names() {
        for role in [Role::Owner, Role::Publisher, Role::Editor, Role::Approver, Role::Auditor] {
            assert_eq!(role_from(role_id(role)), Some(role));
        }
        assert_eq!(role_from("steward"), None);
    }
}
