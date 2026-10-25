//! The agent table's own checks (spec 3.3 §5.7, §10 K7, S-I12): what the kernel knows
//! about a definition without its host — the risk floor, its area's limits, a suggester's
//! ceiling, and whether a change lets an agent do more.

use std::collections::BTreeSet;

use adk_core::ApprovalRisk;
use serde_json::Value;

use crate::agents::factory::{AgentAssessment, AgentCeiling};
use crate::agents::record::{AgentDefinition, AgentOrigin, FileAccess, ShellAccess, WebAccess};
use crate::drivers::EffectClass;
use crate::partition::{Partition, PartitionMode};

/// The packs that let an agent spend money (spec 3.3 §5.7): payments and computer use.
pub const MONEY_PACKS: [&str; 3] = ["payments", "computer-use", "computer_use"];

/// How much a mode lets an agent do without asking: 0 for `read_only` to 3 for `auto`.
pub(crate) fn looseness(mode: PartitionMode) -> u8 {
    match mode {
        PartitionMode::ReadOnly => 0,
        PartitionMode::Plan => 1,
        PartitionMode::Ask => 2,
        PartitionMode::Auto => 3,
    }
}

/// The plain words of a mode, as the surfaces show it.
pub(crate) fn mode_words(mode: PartitionMode) -> &'static str {
    match mode {
        PartitionMode::ReadOnly => "Just look",
        PartitionMode::Plan => "Suggest",
        PartitionMode::Ask => "Ask first",
        PartitionMode::Auto => "Go ahead",
    }
}

/// The risk of a driver whose tools are of `classes` (spec 3.3 §5.7): low for reads,
/// medium for writing locally and scheduling with others, high for sending and deleting and
/// for tools it does not describe, high and money for spending or publishing (K-I9).
fn class_risk(class: EffectClass) -> (ApprovalRisk, bool) {
    match class {
        EffectClass::Read => (ApprovalRisk::Low, false),
        EffectClass::WriteLocal | EffectClass::ScheduleWithOthers => (ApprovalRisk::Medium, false),
        EffectClass::PublishPublic | EffectClass::Financial => (ApprovalRisk::High, true),
        _ => (ApprovalRisk::High, false),
    }
}

fn class_words(class: EffectClass) -> &'static str {
    match class {
        EffectClass::Read => "read",
        EffectClass::WriteLocal => "change your data",
        EffectClass::ScheduleWithOthers => "schedule with others",
        EffectClass::SendExternal => "send to others",
        EffectClass::PublishPublic => "publish publicly",
        EffectClass::Financial => "move money",
        EffectClass::Delete => "delete",
        _ => "do things it does not describe",
    }
}

/// What the kernel itself knows of `definition`'s risk (spec 3.3 §5.7's risk table), the
/// floor under any factory's assessment: its mode, files, shell, web and money packs; each
/// of its drivers by the effect classes of its tools (`drivers`: id and classes; a driver
/// with no class listed counts as undescribed); and a suggestion from work the person did
/// not start at a screen, which is high (decision 20). Abilities name only the drivers; the
/// factory words the rest.
///
/// # Example
///
/// ```rust
/// use adk_core::ApprovalRisk;
/// use adk_kernel::agents::{AgentDefinition, AgentOrigin, AgentUses, kernel_assessment};
/// use adk_kernel::drivers::EffectClass;
/// use adk_kernel::PartitionMode;
///
/// let calendar = AgentDefinition::new("Planner", "work")
///     .with_mode(PartitionMode::Plan)
///     .with_uses(AgentUses::default().with_driver("calendar"));
/// let low = kernel_assessment(&calendar, &AgentOrigin::ByHand, &[("calendar".into(), vec![EffectClass::Read])]);
/// assert_eq!(low.risk, ApprovalRisk::Low);
/// let money = kernel_assessment(&calendar, &AgentOrigin::ByHand, &[("calendar".into(), vec![EffectClass::Financial])]);
/// assert_eq!((money.risk, money.money), (ApprovalRisk::High, true));
/// ```
pub fn kernel_assessment(
    definition: &AgentDefinition,
    origin: &AgentOrigin,
    drivers: &[(String, Vec<EffectClass>)],
) -> AgentAssessment {
    let uses = &definition.uses;
    let mut risk = match definition.mode {
        PartitionMode::ReadOnly | PartitionMode::Plan => ApprovalRisk::Low,
        PartitionMode::Ask => ApprovalRisk::Medium,
        PartitionMode::Auto => ApprovalRisk::High,
    };
    if uses.files == FileAccess::ReadWrite || uses.web == WebAccess::Any {
        risk = risk.max(ApprovalRisk::Medium);
    }
    risk = risk.max(match uses.shell {
        ShellAccess::None => ApprovalRisk::Low,
        ShellAccess::Sandboxed => ApprovalRisk::Medium,
        ShellAccess::Host => ApprovalRisk::High,
    });
    if matches!(origin, AgentOrigin::Suggested { unattended: true, .. }) {
        risk = ApprovalRisk::High;
    }
    let mut assessment = AgentAssessment::new(risk);
    if uses.packs.iter().any(|pack| MONEY_PACKS.contains(&pack.as_str())) {
        assessment = assessment.with_money();
    }
    for driver in &uses.drivers {
        let classes: BTreeSet<EffectClass> = drivers
            .iter()
            .find(|(id, _)| id == driver)
            .map(|(_, classes)| classes.iter().copied().collect())
            .filter(|classes: &BTreeSet<EffectClass>| !classes.is_empty())
            .unwrap_or_else(|| BTreeSet::from([EffectClass::Other]));
        for class in &classes {
            let (class_risk, money) = class_risk(*class);
            assessment.risk = assessment.risk.max(class_risk);
            if money {
                assessment = assessment.with_money();
            }
        }
        let words: Vec<&str> = classes.iter().map(|class| class_words(*class)).collect();
        assessment
            .abilities
            .push(format!("It can use the {driver} driver, which can {}.", join_words(&words)));
    }
    assessment
}

fn join_words(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Why `definition` exceeds its area (spec 3.3 §5.7): a mode looser than the area's, a
/// daily limit above the area's, or a driver not mounted for it. Empty when it fits.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::{AgentDefinition, AgentUses, area_limits};
/// use adk_kernel::{Partition, PartitionMode};
///
/// let home = Partition::new("home", "home").with_mode(PartitionMode::Ask);
/// let ok = AgentDefinition::new("Finance", "home").with_mode(PartitionMode::Plan);
/// assert!(area_limits(&ok, &home).is_empty());
/// let loose = ok.clone().with_mode(PartitionMode::Auto).with_uses(AgentUses::default().with_driver("mail"));
/// assert_eq!(area_limits(&loose, &home).len(), 2);
/// ```
pub fn area_limits(definition: &AgentDefinition, area: &Partition) -> Vec<String> {
    let mut over = Vec::new();
    if looseness(definition.mode) > looseness(area.default_mode) {
        over.push(format!(
            "it would {} where its area asks for {} at most",
            mode_words(definition.mode).to_lowercase(),
            mode_words(area.default_mode).to_lowercase()
        ));
    }
    if let Some(cap) = area.quotas.daily_spend_cap {
        match definition.daily_limit_micro_usd {
            Some(limit) if limit <= cap => {}
            Some(limit) => over.push(format!(
                "its daily limit of {} is above its area's {}",
                dollars(limit),
                dollars(cap)
            )),
            None => {}
        }
    }
    // Mounting a driver is its own approval (K-I13), never part of adding an agent.
    for driver in definition.uses.drivers.iter().filter(|driver| !area.drivers.contains(driver)) {
        over.push(format!("the {driver} driver is not mounted for its area; mount it there first"));
    }
    over
}

/// Micro-USD as the person reads money: `$2.50`.
fn dollars(micro_usd: u64) -> String {
    let cents = micro_usd.saturating_add(5_000) / 10_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// Why a suggested `definition` exceeds its suggester (spec 3.3 S-I12): what is over the
/// suggester's `ceiling`, and what a suggestion never carries — a shell on this computer,
/// payments or computer use. Empty when it fits.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::{AgentCeiling, AgentDefinition, AgentUses, ShellAccess, over_ceiling};
/// use adk_kernel::PartitionMode;
///
/// let ceiling = AgentCeiling::new(PartitionMode::Auto, AgentUses::default().with_shell(ShellAccess::Host));
/// let host = AgentDefinition::new("Ops", "default").with_uses(AgentUses::default().with_shell(ShellAccess::Host));
/// assert_eq!(over_ceiling(&host, &ceiling), ["a shell on this computer"]);
/// ```
pub fn over_ceiling(definition: &AgentDefinition, ceiling: &AgentCeiling) -> Vec<String> {
    let mut over = Vec::new();
    if looseness(definition.mode) > looseness(ceiling.mode) {
        over.push(format!(
            "{} where it may only {}",
            mode_words(definition.mode).to_lowercase(),
            mode_words(ceiling.mode).to_lowercase()
        ));
    }
    over.extend(definition.uses.over(&ceiling.uses));
    let host = "a shell on this computer".to_string();
    if definition.uses.shell == ShellAccess::Host && !over.contains(&host) {
        over.push(host);
    }
    for pack in definition.uses.packs.iter().filter(|pack| MONEY_PACKS.contains(&pack.as_str())) {
        let words = format!("the {pack} pack");
        if !over.contains(&words) {
            over.push(words);
        }
    }
    over
}

/// The definition fields `now` changes from `was`, by their wire names, sorted.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::{AgentDefinition, changed_fields};
/// use adk_kernel::PartitionMode;
///
/// let was = AgentDefinition::new("Finance", "default");
/// let now = was.clone().with_mode(PartitionMode::Plan);
/// let mut renamed = now.clone();
/// renamed.display_name = "Bills".into();
/// assert_eq!(changed_fields(&was, &renamed), ["displayName", "mode"]);
/// ```
pub fn changed_fields(was: &AgentDefinition, now: &AgentDefinition) -> Vec<String> {
    let (Ok(Value::Object(old)), Ok(Value::Object(new))) = (
        serde_json::to_value(was.clone().normalized()),
        serde_json::to_value(now.clone().normalized()),
    ) else {
        return Vec::new();
    };
    old.keys()
        .chain(new.keys())
        .filter(|key| old.get(*key) != new.get(*key))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// What `now` lets an agent do that `was` did not (spec 3.3 §5.7): a looser mode, more
/// uses, a higher or no daily limit. Empty when the change only makes it do less or the
/// same; its name, colour, line, instructions and model never count.
///
/// # Example
///
/// ```rust
/// use adk_kernel::agents::{AgentDefinition, AgentUses, FileAccess, widened};
/// use adk_kernel::PartitionMode;
///
/// let was = AgentDefinition::new("Finance", "default").with_mode(PartitionMode::Ask);
/// assert!(widened(&was, &was.clone().with_mode(PartitionMode::Plan)).is_empty());
/// let more = was.clone().with_uses(AgentUses::default().with_files(FileAccess::Read));
/// assert_eq!(widened(&was, &more), ["reading files"]);
/// ```
pub fn widened(was: &AgentDefinition, now: &AgentDefinition) -> Vec<String> {
    let mut more = Vec::new();
    if looseness(now.mode) > looseness(was.mode) {
        more.push(format!(
            "{} instead of {}",
            mode_words(now.mode).to_lowercase(),
            mode_words(was.mode).to_lowercase()
        ));
    }
    more.extend(now.uses.over(&was.uses));
    match (was.daily_limit_micro_usd, now.daily_limit_micro_usd) {
        (Some(_), None) => more.push("no daily limit of its own".to_string()),
        (Some(old), Some(new)) if new > old => {
            more.push(format!("a daily limit of {new} micro-USD instead of {old}"));
        }
        _ => {}
    }
    more
}

#[cfg(test)]
mod tests {
    use adk_core::AgentId;

    use super::*;
    use crate::agents::record::AgentUses;

    fn definition(mode: PartitionMode, uses: AgentUses) -> AgentDefinition {
        AgentDefinition::new("Finance", "default").with_mode(mode).with_uses(uses)
    }

    fn risk(mode: PartitionMode, uses: AgentUses) -> (ApprovalRisk, bool) {
        let assessment = kernel_assessment(&definition(mode, uses), &AgentOrigin::ByHand, &[]);
        (assessment.risk, assessment.money)
    }

    /// Wave 3 finding 25: what an area refuses reads as plain words, in dollars, with no
    /// internal identifier.
    #[test]
    fn area_refusals_are_plain_words_in_dollars() {
        let mut area = Partition::new("home", "home").with_mode(PartitionMode::Ask);
        area.quotas.daily_spend_cap = Some(2_000_000);
        let definition = definition(PartitionMode::Ask, AgentUses::default().with_driver("mail"))
            .with_daily_limit(5_000_000);
        let over = area_limits(&definition, &area);
        assert_eq!(
            over,
            [
                "its daily limit of $5.00 is above its area's $2.00",
                "the mail driver is not mounted for its area; mount it there first"
            ]
        );
        assert_eq!(dollars(2_499_999), "$2.50");
        assert_eq!(dollars(999_999), "$1.00");
    }

    #[test]
    fn the_risk_table_rows() {
        use ApprovalRisk::{High, Low, Medium};
        let none = AgentUses::default;
        // Low: Just look or Suggest; no shell; no web or listed sites.
        assert_eq!(risk(PartitionMode::ReadOnly, none()), (Low, false));
        assert_eq!(
            risk(
                PartitionMode::Plan,
                none().with_files(FileAccess::Read).with_web(WebAccess::listed(["a.io"]))
            ),
            (Low, false)
        );
        // Medium: Ask first; changing files; the sandboxed shell; any site.
        assert_eq!(risk(PartitionMode::Ask, none()), (Medium, false));
        assert_eq!(
            risk(PartitionMode::Plan, none().with_files(FileAccess::ReadWrite)),
            (Medium, false)
        );
        assert_eq!(
            risk(PartitionMode::Plan, none().with_shell(ShellAccess::Sandboxed)),
            (Medium, false)
        );
        assert_eq!(risk(PartitionMode::Plan, none().with_web(WebAccess::Any)), (Medium, false));
        // High: Go ahead; a shell on this computer.
        assert_eq!(risk(PartitionMode::Auto, none()), (High, false));
        assert_eq!(
            risk(PartitionMode::ReadOnly, none().with_shell(ShellAccess::Host)),
            (High, false)
        );
        // High, spends money: payments or computer use.
        assert_eq!(risk(PartitionMode::ReadOnly, none().with_pack("payments")), (High, true));
        assert_eq!(risk(PartitionMode::ReadOnly, none().with_pack("computer-use")), (High, true));
    }

    #[test]
    fn drivers_count_by_their_effect_classes() {
        let uses = AgentUses::default().with_driver("cal");
        let read_only = definition(PartitionMode::ReadOnly, uses.clone());
        let assess = |classes: Vec<EffectClass>| {
            let a = kernel_assessment(&read_only, &AgentOrigin::ByHand, &[("cal".into(), classes)]);
            (a.risk, a.money)
        };
        assert_eq!(assess(vec![EffectClass::Read]), (ApprovalRisk::Low, false));
        assert_eq!(
            assess(vec![EffectClass::Read, EffectClass::ScheduleWithOthers]),
            (ApprovalRisk::Medium, false)
        );
        assert_eq!(assess(vec![EffectClass::WriteLocal]), (ApprovalRisk::Medium, false));
        assert_eq!(assess(vec![EffectClass::SendExternal]), (ApprovalRisk::High, false));
        assert_eq!(assess(vec![EffectClass::Delete]), (ApprovalRisk::High, false));
        assert_eq!(assess(vec![EffectClass::PublishPublic]), (ApprovalRisk::High, true));
        assert_eq!(assess(vec![EffectClass::Financial]), (ApprovalRisk::High, true));
        // A driver the kernel knows nothing of counts as undescribed.
        assert_eq!(assess(vec![]), (ApprovalRisk::High, false));
        let words = kernel_assessment(
            &read_only,
            &AgentOrigin::ByHand,
            &[("cal".into(), vec![EffectClass::Read, EffectClass::ScheduleWithOthers])],
        );
        assert_eq!(
            words.abilities,
            ["It can use the cal driver, which can read and schedule with others."]
        );
    }

    #[test]
    fn an_unattended_suggestion_is_high_risk() {
        let origin = |unattended| AgentOrigin::Suggested {
            agent: AgentId::new("urn:adk:agent:adk-rust/adk-rust").unwrap(),
            session: "s".into(),
            run: None,
            unattended,
        };
        let plain = definition(PartitionMode::ReadOnly, AgentUses::default());
        assert_eq!(kernel_assessment(&plain, &origin(false), &[]).risk, ApprovalRisk::Low);
        assert_eq!(kernel_assessment(&plain, &origin(true), &[]).risk, ApprovalRisk::High);
    }

    #[test]
    fn a_suggestion_never_carries_a_host_shell_or_money() {
        let generous = AgentCeiling::new(
            PartitionMode::Auto,
            AgentUses::default().with_shell(ShellAccess::Host).with_pack("payments"),
        );
        let wants = definition(
            PartitionMode::Ask,
            AgentUses::default().with_shell(ShellAccess::Host).with_pack("payments"),
        );
        assert_eq!(
            over_ceiling(&wants, &generous),
            ["a shell on this computer", "the payments pack"]
        );
        let tight = AgentCeiling::new(PartitionMode::Plan, AgentUses::default());
        let looser = definition(PartitionMode::Ask, AgentUses::default());
        assert_eq!(over_ceiling(&looser, &tight), ["ask first where it may only suggest"]);
    }

    #[test]
    fn area_limits_cover_mode_limit_and_drivers() {
        let home = Partition::new("home", "home")
            .with_mode(PartitionMode::Ask)
            .with_driver("calendar")
            .with_quotas(crate::sched::Quotas::new().with_daily_spend_cap(1_000));
        let fits = definition(PartitionMode::Ask, AgentUses::default().with_driver("calendar"))
            .with_daily_limit(500);
        assert!(area_limits(&fits, &home).is_empty());
        let over = definition(PartitionMode::Auto, AgentUses::default().with_driver("mail"))
            .with_daily_limit(5_000);
        assert_eq!(area_limits(&over, &home).len(), 3);
    }

    #[test]
    fn widening_ignores_words_and_model() {
        let was = definition(PartitionMode::Ask, AgentUses::default()).with_daily_limit(100);
        let mut words = was.clone().with_instructions("New words").with_description("New line");
        words.display_name = "Bills".into();
        assert!(widened(&was, &words).is_empty());
        let mut unlimited = was.clone();
        unlimited.daily_limit_micro_usd = None;
        assert_eq!(widened(&was, &unlimited), ["no daily limit of its own"]);
        assert_eq!(widened(&was, &was.clone().with_daily_limit(50)), Vec::<String>::new());
    }
}
