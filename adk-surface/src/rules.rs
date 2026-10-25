//! What each surface may decide (spec 3.3 §7.2) and the rules on every surface (§7.4),
//! enforced by the engine rather than left to renderers.
//!
//! | Surface (row) | Allow once | Always allow | Edit | Deny | High risk |
//! |---|---|---|---|---|---|
//! | Web home | Yes | Yes | Yes | Yes | Yes, after confirming it is you |
//! | Terminal | Yes | Yes | Yes | Yes | Yes, on a paired local terminal |
//! | Phone | Yes | Yes | Yes | Yes | Yes, after confirming it is you |
//! | Desktop tray | Yes | No (opens the home) | No | Yes | No (opens the home) |
//! | Chat | Yes, low and medium risk | No | No | Yes | No (links to the home) |
//! | Voice | Yes, low and medium risk, with a cancel window | No | No | Yes | No |
//!
//! The row comes from how the input was given before the surface it arrived on: a spoken
//! answer in the web home follows the voice row, a chat message the chat row, and an agent's
//! request decides nothing at all (§7.4 rule 6). A call that spends money follows the
//! high-risk column and is never allowed "always" (rule 4). The high-risk column is about
//! approving: Deny stays on the tray and in voice for every decision, since saying no is
//! always safe, and only chat, where a high-risk decision never resolves (S-I6), loses it. A
//! call shown truncated cannot be approved until it was expanded (§7.1).

use adk_core::ApprovalAction;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::origin::{Caller, Origin, SurfaceKind};
use crate::words;

/// How long a spoken "allow once" waits before it takes effect, so the person can cancel it
/// on screen (spec 3.3 §5.6, §7.2).
pub const VOICE_CANCEL_WINDOW_SECS: u32 = 5;

/// An answer to a decision (spec 3.3 §8.2).
///
/// # Example
///
/// ```rust
/// use adk_surface::Answer;
///
/// assert_eq!(serde_json::to_value(Answer::AllowOnce).unwrap(), "allowOnce");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Answer {
    /// Run this one call.
    AllowOnce,
    /// Run it and allow the same call from now on, scoped.
    AlwaysAllow,
    /// Change the arguments, then allow.
    Edit,
    /// Refuse.
    Deny,
}

/// How much harm a call can do, as approvers see it (spec 3.3 §7.1).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum Risk {
    /// Routine.
    Low,
    /// Needs a look.
    Medium,
    /// Needs a careful look.
    High,
}

impl Risk {
    /// The words: `"Low risk"`, `"Medium risk"`, `"High risk"`.
    pub fn words(self) -> &'static str {
        match self {
            Self::Low => words::LOW_RISK,
            Self::Medium => words::MEDIUM_RISK,
            Self::High => words::HIGH_RISK,
        }
    }
}

impl From<adk_core::ApprovalRisk> for Risk {
    fn from(risk: adk_core::ApprovalRisk) -> Self {
        match risk {
            adk_core::ApprovalRisk::Low => Self::Low,
            adk_core::ApprovalRisk::Medium => Self::Medium,
            // A critical call needs at least the care of a high-risk one.
            adk_core::ApprovalRisk::High | adk_core::ApprovalRisk::Critical => Self::High,
        }
    }
}

/// The row of the §7.2 table that applies to a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Row {
    /// The web home.
    Web,
    /// The terminal.
    Terminal,
    /// The phone.
    Phone,
    /// The desktop tray.
    Tray,
    /// A chat channel.
    Chat,
    /// Voice.
    Voice,
    /// An agent: decides nothing (§7.4 rule 6).
    Agent,
}

/// The row that applies to `caller`: the way the input was given first, then the surface.
///
/// # Example
///
/// ```rust
/// use adk_surface::{Caller, Origin, SurfaceKind};
/// use adk_surface::rules::{Row, row};
///
/// let spoken = Caller::human("ana", SurfaceKind::Web, Origin::Voice);
/// assert_eq!(row(&spoken), Row::Voice);
/// let typed = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard);
/// assert_eq!(row(&typed), Row::Terminal);
/// ```
pub fn row(caller: &Caller) -> Row {
    match &caller.origin {
        Origin::Agent { .. } => Row::Agent,
        Origin::Voice => Row::Voice,
        Origin::Chat { .. } => Row::Chat,
        Origin::Keyboard | Origin::Pointer | Origin::Touch => match caller.surface {
            SurfaceKind::Web => Row::Web,
            SurfaceKind::Terminal => Row::Terminal,
            SurfaceKind::Phone => Row::Phone,
            SurfaceKind::Tray => Row::Tray,
            SurfaceKind::Chat => Row::Chat,
            SurfaceKind::Voice => Row::Voice,
        },
    }
}

/// What the engine knows about a decision when it computes the answers (spec 3.3 §7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionFacts {
    /// The risk the inbox entry carries.
    pub risk: Risk,
    /// The call spends money: never "always", never approved from chat, voice or the tray
    /// (§7.4 rule 4).
    pub money: bool,
    /// The call is shown truncated and must be expanded before it is approved (§7.1).
    pub truncated: bool,
    /// What the inbox entry allows.
    pub allows: Vec<ApprovalAction>,
    /// The label of "Always allow" when the source can create the standing rule, such as
    /// "Always allow for this person"; `None` when it cannot.
    pub always_label: Option<String>,
    /// The decision adds an agent a person was suggested (`kernel:agent.create`): it follows
    /// the "Always allow" column, with **Add this agent** and **Don't add** (spec 3.3 §5.7,
    /// §7.2).
    pub adds_agent: bool,
}

/// One answer a surface may offer, with its label (spec 3.3 §8.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnswerOption {
    /// The answer.
    pub answer: Answer,
    /// The button's words.
    pub label: String,
    /// The person must confirm it is them first (high risk on the web home and the phone).
    pub needs_presence: bool,
    /// The call must be expanded first: it is shown truncated.
    pub needs_expanded: bool,
}

/// The answers a surface may give one decision (spec 3.3 §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Allowed {
    /// The answers, in the order of §7.1: allow once, always allow, edit, deny.
    pub options: Vec<AnswerOption>,
    /// Some answers are only in the home: show "Open in home".
    pub open_in_home: bool,
    /// A spoken "allow once" takes effect after this many seconds unless cancelled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_window_secs: Option<u32>,
    /// Why this surface cannot approve, in plain words, when it cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Allowed {
    /// The option for `answer`, when this surface offers it.
    pub fn option(&self, answer: Answer) -> Option<&AnswerOption> {
        self.options.iter().find(|option| option.answer == answer)
    }
}

/// The answers `caller` may give a decision with `facts` (spec 3.3 §7.2, §7.4).
///
/// # Example
///
/// ```rust
/// use adk_core::ApprovalAction;
/// use adk_surface::{Answer, Caller, Origin, SurfaceKind};
/// use adk_surface::rules::{DecisionFacts, Risk, allowed_answers};
///
/// let facts = DecisionFacts {
///     risk: Risk::Medium,
///     money: false,
///     truncated: false,
///     allows: ApprovalAction::ALL.to_vec(),
///     always_label: Some("Always allow for this person".into()),
///     adds_agent: false,
/// };
/// let chat = Caller::human("ana", SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() });
/// let answers: Vec<Answer> = allowed_answers(&chat, &facts).options.iter().map(|o| o.answer).collect();
/// assert_eq!(answers, vec![Answer::AllowOnce, Answer::Deny]);
///
/// let high = DecisionFacts { risk: Risk::High, ..facts };
/// assert!(allowed_answers(&chat, &high).options.is_empty());
/// ```
pub fn allowed_answers(caller: &Caller, facts: &DecisionFacts) -> Allowed {
    let row = row(caller);
    if facts.adds_agent {
        return agent_answers(row, caller, facts);
    }
    let consequential = facts.risk == Risk::High || facts.money;
    let can = |action: ApprovalAction| facts.allows.contains(&action);
    let mut allowed =
        Allowed { options: Vec::new(), open_in_home: false, cancel_window_secs: None, note: None };
    let push = |allowed: &mut Allowed, answer: Answer, label: &str, needs_presence: bool| {
        let needs_expanded = facts.truncated && answer != Answer::Deny;
        allowed.options.push(AnswerOption {
            answer,
            label: label.to_string(),
            needs_presence,
            needs_expanded,
        });
    };
    match row {
        Row::Agent => {
            allowed.note = Some(words::NEEDS_A_SCREEN.to_string());
        }
        Row::Web | Row::Phone | Row::Terminal => {
            // High risk: after confirming it is you (web home, phone), or on a paired local
            // terminal; a terminal that is neither sends the person to the home.
            let high_ok = !consequential || row != Row::Terminal || caller.paired_local;
            let needs_presence = consequential && row != Row::Terminal;
            if high_ok && can(ApprovalAction::Approve) {
                let label = if needs_presence {
                    words::CONFIRM_ITS_YOU_AND_APPROVE
                } else {
                    words::ALLOW_ONCE
                };
                push(&mut allowed, Answer::AllowOnce, label, needs_presence);
            }
            if !consequential
                && can(ApprovalAction::Approve)
                && let Some(label) = &facts.always_label
            {
                push(&mut allowed, Answer::AlwaysAllow, label, false);
            }
            if high_ok && can(ApprovalAction::Edit) {
                push(&mut allowed, Answer::Edit, words::EDIT, needs_presence);
            }
            if can(ApprovalAction::Reject) {
                push(&mut allowed, Answer::Deny, words::DENY, false);
            }
            if !high_ok {
                allowed.open_in_home = true;
                allowed.note = Some(words::APPROVE_IN_THE_HOME.to_string());
            }
        }
        Row::Tray | Row::Chat | Row::Voice => {
            if consequential {
                // Saying no is always safe, so the tray and voice keep Deny; a high-risk
                // decision never resolves in chat (S-I6), and chat never touches money (§7.4
                // rule 4).
                if row != Row::Chat && can(ApprovalAction::Reject) {
                    push(&mut allowed, Answer::Deny, words::DENY, false);
                }
                allowed.open_in_home = row != Row::Voice;
                allowed.note = Some(
                    match row {
                        Row::Tray => words::APPROVE_IN_THE_HOME,
                        _ => words::NEEDS_A_SCREEN,
                    }
                    .to_string(),
                );
            } else {
                if can(ApprovalAction::Approve) {
                    push(&mut allowed, Answer::AllowOnce, words::ALLOW_ONCE, false);
                }
                if can(ApprovalAction::Reject) {
                    push(&mut allowed, Answer::Deny, words::DENY, false);
                }
                // Always allow and edit live in the home (the tray opens it, chat links it).
                allowed.open_in_home = row != Row::Voice;
                if row == Row::Voice {
                    allowed.cancel_window_secs = Some(VOICE_CANCEL_WINDOW_SECS);
                }
            }
        }
    }
    allowed
}

/// The answers to a suggested agent (spec 3.3 §5.7, §7.2 "Add an agent, or let one do
/// more"): adding is a standing change, so it follows "Always allow" — the web home, the phone
/// and the terminal, at high risk after confirming it is you or on a paired local terminal —
/// with no Always allow and no Edit; **Don't add** is the Deny column, on every surface.
fn agent_answers(row: Row, caller: &Caller, facts: &DecisionFacts) -> Allowed {
    use crate::agents::words as agent_words;
    let consequential = facts.risk == Risk::High || facts.money;
    let can = |action: ApprovalAction| facts.allows.contains(&action);
    let mut allowed =
        Allowed { options: Vec::new(), open_in_home: false, cancel_window_secs: None, note: None };
    let option = |answer: Answer, label: &str, needs_presence: bool| AnswerOption {
        answer,
        label: label.to_string(),
        needs_presence,
        needs_expanded: facts.truncated && answer != Answer::Deny,
    };
    match row {
        Row::Agent => allowed.note = Some(words::NEEDS_A_SCREEN.to_string()),
        Row::Web | Row::Phone | Row::Terminal => {
            let high_ok = !consequential || row != Row::Terminal || caller.paired_local;
            let needs_presence = consequential && row != Row::Terminal;
            if high_ok && can(ApprovalAction::Approve) {
                let label = if needs_presence {
                    agent_words::CONFIRM_ITS_YOU_AND_ADD
                } else {
                    agent_words::ADD_THIS_AGENT
                };
                allowed.options.push(option(Answer::AllowOnce, label, needs_presence));
            }
            if can(ApprovalAction::Reject) {
                allowed.options.push(option(Answer::Deny, agent_words::DONT_ADD, false));
            }
            if !high_ok {
                allowed.open_in_home = true;
                allowed.note = Some(words::APPROVE_IN_THE_HOME.to_string());
            }
        }
        Row::Tray | Row::Chat | Row::Voice => {
            if can(ApprovalAction::Reject) {
                allowed.options.push(option(Answer::Deny, agent_words::DONT_ADD, false));
            }
            allowed.open_in_home = row != Row::Voice;
            allowed.note = Some(
                match row {
                    Row::Voice => words::NEEDS_A_SCREEN,
                    _ => words::APPROVE_IN_THE_HOME,
                }
                .to_string(),
            );
        }
    }
    allowed
}

/// Why the engine refused an answer (spec 3.3 §7.2, §7.4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleViolation {
    /// The surface may not give this answer to this decision.
    #[error("this answer is not available here; open the decision in the home app to answer it")]
    NotOffered,
    /// The answer needs the person to confirm it is them first.
    #[error(
        "confirm it's you first: high-risk calls are approved only after a passkey or device check"
    )]
    PresenceRequired,
    /// The call was shown truncated and was not expanded.
    #[error("expand the call first: a truncated call cannot be approved until it is shown in full")]
    NotExpanded,
}

impl RuleViolation {
    /// The error code the engine reports.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotOffered => "surface.answer_not_allowed",
            Self::PresenceRequired => "surface.confirm_presence",
            Self::NotExpanded => "surface.call_not_expanded",
        }
    }
}

/// Checks that `caller` may give `answer` to a decision with `facts`, `expanded` saying
/// whether the surface showed the call in full (spec 3.3 §7.1, §7.2, §7.4).
///
/// # Errors
///
/// Returns the [`RuleViolation`] that refuses the answer.
///
/// # Example
///
/// ```rust
/// use adk_core::ApprovalAction;
/// use adk_surface::{Answer, Caller, Origin, SurfaceKind};
/// use adk_surface::rules::{DecisionFacts, Risk, RuleViolation, check};
///
/// let facts = DecisionFacts {
///     risk: Risk::High,
///     money: false,
///     truncated: false,
///     allows: ApprovalAction::ALL.to_vec(),
///     always_label: None,
///     adds_agent: false,
/// };
/// let web = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
/// assert_eq!(check(&web, &facts, Answer::AllowOnce, true), Err(RuleViolation::PresenceRequired));
/// assert!(check(&web.with_presence_confirmed(), &facts, Answer::AllowOnce, true).is_ok());
/// ```
pub fn check(
    caller: &Caller,
    facts: &DecisionFacts,
    answer: Answer,
    expanded: bool,
) -> Result<(), RuleViolation> {
    let allowed = allowed_answers(caller, facts);
    let option = allowed.option(answer).ok_or(RuleViolation::NotOffered)?;
    if option.needs_presence && !caller.presence_confirmed {
        return Err(RuleViolation::PresenceRequired);
    }
    if option.needs_expanded && !expanded {
        return Err(RuleViolation::NotExpanded);
    }
    Ok(())
}

/// Whether `caller` may resume after a stop: only the web home or a paired local terminal,
/// with a person's own hands (spec 3.3 §8.2, §11 S-I10).
///
/// # Example
///
/// ```rust
/// use adk_surface::{Caller, Origin, SurfaceKind};
/// use adk_surface::rules::may_resume;
///
/// assert!(may_resume(&Caller::human("ana", SurfaceKind::Web, Origin::Pointer)));
/// assert!(!may_resume(&Caller::human("ana", SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() })));
/// assert!(!may_resume(&Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard)));
/// ```
pub fn may_resume(caller: &Caller) -> bool {
    match row(caller) {
        Row::Web => true,
        Row::Terminal => caller.paired_local,
        Row::Phone | Row::Tray | Row::Chat | Row::Voice | Row::Agent => false,
    }
}

/// Whether `caller` may make agents act more on their own (a looser mode), which is a
/// standing permission and so follows "Always allow" (spec 3.3 §7.2, §7.4 rule 4): the web
/// home, the phone and the terminal. Any surface may tighten.
///
/// # Example
///
/// ```rust
/// use adk_surface::{Caller, Origin, SurfaceKind};
/// use adk_surface::rules::may_loosen_mode;
///
/// assert!(may_loosen_mode(&Caller::human("ana", SurfaceKind::Phone, Origin::Touch)));
/// assert!(!may_loosen_mode(&Caller::human("ana", SurfaceKind::Web, Origin::Voice)));
/// ```
pub fn may_loosen_mode(caller: &Caller) -> bool {
    matches!(row(caller), Row::Web | Row::Phone | Row::Terminal)
}

/// Whether `caller` may add, change, pause or remove an agent here (spec 3.3 §5.7, §7.2): a
/// person at the web home or the terminal. The phone answers suggestions only (decision 16).
///
/// # Example
///
/// ```rust
/// use adk_surface::{Caller, Origin, SurfaceKind};
/// use adk_surface::rules::may_change_agents;
///
/// assert!(may_change_agents(&Caller::human("ana", SurfaceKind::Web, Origin::Pointer)));
/// assert!(may_change_agents(&Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard)));
/// assert!(!may_change_agents(&Caller::human("ana", SurfaceKind::Phone, Origin::Touch)));
/// ```
pub fn may_change_agents(caller: &Caller) -> bool {
    matches!(row(caller), Row::Web | Row::Terminal) && caller.device.is_none()
}

/// Whether `caller` may resume one agent paused by hand: where Resume all may be given (spec
/// 3.3 §5.7, S-I10).
///
/// # Example
///
/// ```rust
/// use adk_surface::{Caller, Origin, SurfaceKind};
/// use adk_surface::rules::may_resume_agent;
///
/// assert!(may_resume_agent(&Caller::human("ana", SurfaceKind::Web, Origin::Pointer)));
/// assert!(!may_resume_agent(&Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard)));
/// ```
pub fn may_resume_agent(caller: &Caller) -> bool {
    may_resume(caller)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(risk: Risk) -> DecisionFacts {
        DecisionFacts {
            risk,
            money: false,
            truncated: false,
            allows: ApprovalAction::ALL.to_vec(),
            always_label: Some(words::ALWAYS_ALLOW_FOR_THIS_PERSON.into()),
            adds_agent: false,
        }
    }

    fn answers(caller: &Caller, facts: &DecisionFacts) -> Vec<Answer> {
        allowed_answers(caller, facts).options.iter().map(|option| option.answer).collect()
    }

    fn caller(surface: SurfaceKind, origin: Origin) -> Caller {
        Caller::human("ana", surface, origin)
    }

    fn telegram() -> Origin {
        Origin::Chat { channel: "Telegram".into() }
    }

    use Answer::{AllowOnce, AlwaysAllow, Deny, Edit};

    /// The §7.2 table, row by row, for low/medium and high risk.
    #[test]
    fn the_table_of_section_7_2() {
        let everyday = facts(Risk::Medium);
        let high = facts(Risk::High);
        let web = caller(SurfaceKind::Web, Origin::Pointer);
        let phone = caller(SurfaceKind::Phone, Origin::Touch);
        let terminal = caller(SurfaceKind::Terminal, Origin::Keyboard);
        let paired = caller(SurfaceKind::Terminal, Origin::Keyboard).with_paired_local();
        let tray = caller(SurfaceKind::Tray, Origin::Pointer);
        let chat = caller(SurfaceKind::Chat, telegram());
        let voice = caller(SurfaceKind::Voice, Origin::Voice);
        let agent = caller(SurfaceKind::Web, Origin::Agent { agent: "finance".into() });

        for full in [&web, &phone, &terminal] {
            assert_eq!(answers(full, &everyday), vec![AllowOnce, AlwaysAllow, Edit, Deny]);
        }
        assert_eq!(answers(&tray, &everyday), vec![AllowOnce, Deny]);
        assert_eq!(answers(&chat, &everyday), vec![AllowOnce, Deny]);
        assert_eq!(answers(&voice, &everyday), vec![AllowOnce, Deny]);
        assert!(answers(&agent, &everyday).is_empty());

        // High risk: never "always"; web and phone after confirming it is you; a paired local
        // terminal; nothing but the home elsewhere.
        for screen in [&web, &phone] {
            let allowed = allowed_answers(screen, &high);
            assert_eq!(answers(screen, &high), vec![AllowOnce, Edit, Deny]);
            assert_eq!(allowed.options[0].label, words::CONFIRM_ITS_YOU_AND_APPROVE);
            assert!(allowed.options[0].needs_presence && allowed.options[1].needs_presence);
        }
        assert_eq!(answers(&paired, &high), vec![AllowOnce, Edit, Deny]);
        assert_eq!(answers(&terminal, &high), vec![Deny]);
        let unpaired = allowed_answers(&terminal, &high);
        assert!(unpaired.open_in_home);
        // An unpaired terminal says where the call can be approved; it is at a screen already.
        assert_eq!(unpaired.note.as_deref(), Some(words::APPROVE_IN_THE_HOME));
        // Deny is always safe: the tray and voice keep it on every decision; chat never
        // resolves a high-risk one (S-I6).
        assert_eq!(answers(&tray, &high), vec![Deny]);
        assert_eq!(answers(&voice, &high), vec![Deny]);
        assert!(answers(&chat, &high).is_empty());
        assert!(allowed_answers(&tray, &high).open_in_home);
        assert!(allowed_answers(&chat, &high).open_in_home);
        assert!(!allowed_answers(&voice, &high).open_in_home);
        assert_eq!(allowed_answers(&voice, &high).note.as_deref(), Some(words::NEEDS_A_SCREEN));
        assert_eq!(allowed_answers(&voice, &high).cancel_window_secs, None);
        assert_eq!(allowed_answers(&voice, &everyday).cancel_window_secs, Some(5));
    }

    #[test]
    fn money_is_never_always_and_never_approved_from_chat_or_voice() {
        let money = DecisionFacts { money: true, ..facts(Risk::Low) };
        let web = caller(SurfaceKind::Web, Origin::Pointer);
        assert_eq!(answers(&web, &money), vec![AllowOnce, Edit, Deny]);
        assert!(answers(&caller(SurfaceKind::Chat, telegram()), &money).is_empty());
        assert_eq!(answers(&caller(SurfaceKind::Web, Origin::Voice), &money), vec![Deny]);
    }

    #[test]
    fn a_truncated_call_is_approved_only_once_expanded() {
        let truncated = DecisionFacts { truncated: true, ..facts(Risk::Low) };
        let web = caller(SurfaceKind::Web, Origin::Pointer);
        assert_eq!(check(&web, &truncated, AllowOnce, false), Err(RuleViolation::NotExpanded));
        assert_eq!(check(&web, &truncated, AllowOnce, true), Ok(()));
        assert_eq!(check(&web, &truncated, Deny, false), Ok(()));
    }

    #[test]
    fn the_request_limits_the_answers() {
        let no_edit = DecisionFacts {
            allows: vec![ApprovalAction::Approve, ApprovalAction::Reject],
            always_label: None,
            ..facts(Risk::Low)
        };
        assert_eq!(
            answers(&caller(SurfaceKind::Web, Origin::Pointer), &no_edit),
            vec![AllowOnce, Deny]
        );
    }

    #[test]
    fn resuming_is_the_home_or_a_paired_local_terminal() {
        assert!(may_resume(&caller(SurfaceKind::Web, Origin::Keyboard)));
        assert!(may_resume(&caller(SurfaceKind::Terminal, Origin::Keyboard).with_paired_local()));
        for other in [
            caller(SurfaceKind::Phone, Origin::Touch),
            caller(SurfaceKind::Tray, Origin::Pointer),
            caller(SurfaceKind::Chat, telegram()),
            caller(SurfaceKind::Web, Origin::Voice),
            caller(SurfaceKind::Web, Origin::Agent { agent: "a".into() }),
        ] {
            assert!(!may_resume(&other), "{other:?}");
        }
    }

    /// The §7.2 column "Add an agent, or let one do more" over every row (spec 3.3 §5.7).
    #[test]
    fn a_suggested_agent_follows_the_always_allow_column() {
        use crate::agents::words::{ADD_THIS_AGENT, CONFIRM_ITS_YOU_AND_ADD, DONT_ADD};
        let agent_facts =
            |risk| DecisionFacts { adds_agent: true, always_label: None, ..facts(risk) };
        let labels = |caller: &Caller, facts: &DecisionFacts| -> Vec<(Answer, String, bool)> {
            allowed_answers(caller, facts)
                .options
                .into_iter()
                .map(|option| (option.answer, option.label, option.needs_presence))
                .collect()
        };
        let web = Caller::human("ana", SurfaceKind::Web, Origin::Pointer);
        let phone = Caller::human("ana", SurfaceKind::Phone, Origin::Touch);
        let terminal = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard);
        let local = terminal.clone().with_paired_local();
        let tray = Caller::human("ana", SurfaceKind::Tray, Origin::Pointer);
        let chat =
            Caller::human("ana", SurfaceKind::Chat, Origin::Chat { channel: "Telegram".into() });
        let voice = Caller::human("ana", SurfaceKind::Web, Origin::Voice);
        let agent =
            Caller::human("ana", SurfaceKind::Web, Origin::Agent { agent: "finance".into() });
        let add = (Answer::AllowOnce, ADD_THIS_AGENT.to_string(), false);
        let confirm = (Answer::AllowOnce, CONFIRM_ITS_YOU_AND_ADD.to_string(), true);
        let dont = (Answer::Deny, DONT_ADD.to_string(), false);
        let medium = agent_facts(Risk::Medium);
        let high = agent_facts(Risk::High);
        // No Always allow and no Edit anywhere.
        for caller in [&web, &phone, &terminal] {
            assert_eq!(labels(caller, &medium), [add.clone(), dont.clone()], "{caller:?}");
        }
        assert_eq!(labels(&web, &high), [confirm.clone(), dont.clone()]);
        assert_eq!(labels(&phone, &high), [confirm, dont.clone()]);
        assert_eq!(labels(&terminal, &high), std::slice::from_ref(&dont));
        assert!(allowed_answers(&terminal, &high).open_in_home);
        assert_eq!(labels(&local, &high), [add, dont.clone()]);
        for caller in [&tray, &chat, &voice] {
            for facts in [&medium, &high] {
                assert_eq!(labels(caller, facts), std::slice::from_ref(&dont), "{caller:?}");
            }
        }
        assert!(allowed_answers(&chat, &medium).open_in_home);
        assert!(labels(&agent, &medium).is_empty());
        // Money is high risk too.
        let money = DecisionFacts { money: true, ..agent_facts(Risk::Low) };
        assert_eq!(labels(&terminal, &money), [dont]);
    }
}
