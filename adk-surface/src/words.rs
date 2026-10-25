//! The words every surface shows (spec 3.3 §8).
//!
//! Every on-screen string the engine projects comes from this module, so the web home, the
//! terminal, chat, the phone, the tray and voice say the same thing in the same way:
//! plain words, sentence case, the outcome named in the heading and the verb on the button,
//! "your agents", and no internal identifiers (§8). Renderers may add technical detail on the
//! terminal and developer panels, never in place of these words.
//!
//! | Concept (§8.1) | Constant |
//! |---|---|
//! | An agent doing work | [`WORKING`] |
//! | An agent stopped on a decision | [`WAITING_FOR_YOU`] |
//! | An agent idle until a wake | [`asleep_until`] |
//! | An agent finished | [`DONE`], [`FAILED`] |
//! | The list of pending decisions | [`DECISIONS`] |
//! | A decision answered elsewhere | [`DECIDED`], [`said`] |
//! | A question from the kernel | [`QUESTION`], [`IS_THAT_RIGHT`] |
//! | Something an agent offers to do | [`SUGGESTION`], [`DO_IT`], [`NOT_NOW`] |
//! | The overnight summary | [`WHILE_YOU_WERE_AWAY`] |
//! | The record of actions | [`WHAT_YOUR_AGENTS_DID`], [`EVERY_ACTION_ON_RECORD`] |
//! | Memory | [`WHAT_YOUR_AGENTS_REMEMBER`], [`YOU_SAID_THIS`], [`A_GUESS`] |
//! | How much agents act alone | [`JUST_LOOK`], [`SUGGEST`], [`ASK_FIRST`], [`GO_AHEAD`] |
//! | Things allowed without asking | [`ALLOWED_WITHOUT_ASKING`], [`EXTEND`], [`STOP_ALLOWING`] |
//! | No proactive work, no notifications | [`QUIET_TIME`] |
//! | The spending limit | [`DAILY_SPENDING_LIMIT`] |
//! | Separate parts of a life | [`AREAS`] |
//! | Phones and apps allowed to decide | [`WHERE_YOU_CAN_SAY_YES_FROM`], [`ADD_A_PHONE_OR_APP`] |
//!
//! # Example
//!
//! ```rust
//! use adk_surface::words;
//!
//! assert_eq!(words::WAITING_FOR_YOU, "Waiting for you");
//! assert_eq!(words::asleep_until("07:00"), "Asleep until 07:00");
//! assert_eq!(words::ALLOW_ONCE, "Allow once");
//! ```

// ---------------------------------------------------------------------------
// §8.1 Terms
// ---------------------------------------------------------------------------

/// An agent doing work (`RunState::Running`).
pub const WORKING: &str = "Working";
/// An agent stopped on a decision, and a decision waiting for an answer.
pub const WAITING_FOR_YOU: &str = "Waiting for you";
/// A held call waiting on an answer the person cannot give from here: it was routed to
/// other approvers, or to no one.
pub const WAITING_FOR_AN_ANSWER: &str = "Waiting for an answer";
/// An agent idle with no wake time to show, such as one a stop parked.
pub const ASLEEP: &str = "Asleep";
/// An agent finished (`Completed`).
pub const DONE: &str = "Done";
/// An agent finished with an error (`Failed`).
pub const FAILED: &str = "Failed";
/// The list of pending decisions; always shown with a count badge.
pub const DECISIONS: &str = "Decisions";
/// A decision answered, here or elsewhere.
pub const DECIDED: &str = "Decided";
/// A question from the kernel's `/proc` observations.
pub const QUESTION: &str = "Question";
/// What a question asks.
pub const IS_THAT_RIGHT: &str = "Is that right?";
/// Something an agent offers to do.
pub const SUGGESTION: &str = "Suggestion";
/// Accepts a suggestion.
pub const DO_IT: &str = "Do it";
/// Declines a suggestion for now.
pub const NOT_NOW: &str = "Not now";
/// The overnight summary.
pub const WHILE_YOU_WERE_AWAY: &str = "While you were away";
/// The record of actions.
pub const WHAT_YOUR_AGENTS_DID: &str = "What your agents did";
/// The record check, when it passes.
pub const EVERY_ACTION_ON_RECORD: &str = "Every action is on record";
/// Memory.
pub const WHAT_YOUR_AGENTS_REMEMBER: &str = "What your agents remember";
/// A known fact.
pub const YOU_SAID_THIS: &str = "You said this";
/// An assumed fact.
pub const A_GUESS: &str = "A guess";
/// `HarnessMode::ReadOnly`.
pub const JUST_LOOK: &str = "Just look";
/// `HarnessMode::Plan`.
pub const SUGGEST: &str = "Suggest";
/// `HarnessMode::Ask`.
pub const ASK_FIRST: &str = "Ask first";
/// `HarnessMode::Auto`.
pub const GO_AHEAD: &str = "Go ahead";
/// Standing rules.
pub const ALLOWED_WITHOUT_ASKING: &str = "Allowed without asking";
/// Extends a standing rule.
pub const EXTEND: &str = "Extend";
/// Ends a standing rule.
pub const STOP_ALLOWING: &str = "Stop allowing";
/// Quiet hours.
pub const QUIET_TIME: &str = "Quiet time";
/// The run budget and spend ledger's daily cap.
pub const DAILY_SPENDING_LIMIT: &str = "Daily spending limit";
/// Partitions.
pub const AREAS: &str = "Areas";
/// Paired devices.
pub const WHERE_YOU_CAN_SAY_YES_FROM: &str = "Where you can say yes from";
/// Pairs a device.
pub const ADD_A_PHONE_OR_APP: &str = "Add a phone or app";
/// Who did something when no one agent did: the kernel's own work, such as a question it
/// asks.
pub const YOUR_AGENTS: &str = "Your agents";
/// A suggestion in "While you were away" that names no one agent.
pub const AGENTS_SUGGEST: &str = "Your agents suggest something. Nothing happens until you say so.";
/// A question from the kernel in "While you were away", before its own words are known.
pub const NOTICED_SOMETHING: &str = "Your agents noticed something. Is that right?";
/// Everything resumed after a stop, in "While you were away".
pub const ALL_AGENTS_RESUMED: &str = "All agents resumed.";
/// When a suggestion with no wake of its own runs once allowed.
pub const RIGHT_AWAY: &str = "right away";

/// A suggestion the person added themselves (spec 3.3 §8.1 "Suggestion").
pub const YOU_SUGGESTED_THIS: &str = "You suggested this.";

/// Who suggests a piece of work on a suggestion's card (spec 3.3 §8.1 "Suggestion"): an
/// agent by name, or `None` for the kernel's own work, which no one agent did.
///
/// # Example
///
/// ```rust
/// use adk_surface::words;
///
/// assert_eq!(words::suggested_by(Some("Finance")), "Finance suggests this.");
/// assert_eq!(words::suggested_by(None), "Your agents suggest this.");
/// ```
pub fn suggested_by(agent: Option<&str>) -> String {
    match agent {
        Some(agent) => format!("{agent} suggests this."),
        None => format!("{YOUR_AGENTS} suggest this."),
    }
}

/// When a suggestion runs once allowed: `when` is [`RIGHT_AWAY`], `"every 5 minutes"`,
/// `"at 07:30"` and the like (spec 3.3 §8.1 "Suggestion").
///
/// # Example
///
/// ```rust
/// assert_eq!(
///     adk_surface::words::once_you_say_yes("every 5 minutes"),
///     "Once you say yes, it runs every 5 minutes."
/// );
/// ```
pub fn once_you_say_yes(when: &str) -> String {
    format!("Once you say yes, it runs {when}.")
}

/// How many steps a suggestion's plan has.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::plan_of(3), "Its plan has 3 steps.");
/// assert_eq!(adk_surface::words::plan_of(1), "Its plan has 1 step.");
/// ```
pub fn plan_of(steps: usize) -> String {
    if steps == 1 {
        "Its plan has 1 step.".to_string()
    } else {
        format!("Its plan has {steps} steps.")
    }
}

/// The most a suggestion may spend, in micro-USD.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::may_spend(500_000), "It may spend up to $0.50.");
/// ```
pub fn may_spend(micro_usd: u64) -> String {
    format!("It may spend up to {}.", dollars(micro_usd))
}

/// The heading of a driver mount a hosted definition asks for: what it lets agents use.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::start_driver("email"), "Start email for your agents");
/// ```
pub fn start_driver(id: &str) -> String {
    format!("Start {id} for your agents")
}

/// What a driver mount runs, on its card.
///
/// # Example
///
/// ```rust
/// assert_eq!(
///     adk_surface::words::driver_starts("mcp-email"),
///     "It runs mcp-email on the computer your agents run on."
/// );
/// ```
pub fn driver_starts(command: &str) -> String {
    format!("It runs {command} on the computer your agents run on.")
}

/// An agent asleep until a local time, `"Asleep until 07:00"`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::asleep_until("07:00"), "Asleep until 07:00");
/// ```
pub fn asleep_until(hh_mm: &str) -> String {
    format!("Asleep until {hh_mm}")
}

/// How a mode reads as a status, `"Asks first"`: the third-person form the status line and
/// a thread's header use for the §8.1 mode words.
///
/// # Example
///
/// ```rust
/// use adk_surface::words;
///
/// assert_eq!(words::mode_status(words::ASK_FIRST), "Asks first");
/// assert_eq!(words::mode_status(words::GO_AHEAD), "Goes ahead");
/// ```
pub fn mode_status(mode_word: &str) -> String {
    match mode_word {
        JUST_LOOK => "Just looks".to_string(),
        SUGGEST => "Suggests".to_string(),
        ASK_FIRST => "Asks first".to_string(),
        GO_AHEAD => "Goes ahead".to_string(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// §8.2 Actions
// ---------------------------------------------------------------------------

/// Runs this one call.
pub const ALLOW_ONCE: &str = "Allow once";
/// Allows from now on, for this person.
pub const ALWAYS_ALLOW_FOR_THIS_PERSON: &str = "Always allow for this person";
/// Allows from now on, for this folder.
pub const ALWAYS_ALLOW_FOR_THIS_FOLDER: &str = "Always allow for this folder";
/// Allows from now on, for this command.
pub const ALWAYS_ALLOW_FOR_THIS_COMMAND: &str = "Always allow for this command";
/// Allows from now on, for this site.
pub const ALWAYS_ALLOW_FOR_THIS_SITE: &str = "Always allow for this site";

/// "Always allow …" completed with what the standing rule covers, so the button never
/// promises less than the rule allows (spec 3.3 §8.2, spec 3.1 §7.5): `label` is one of the
/// `ALWAYS_ALLOW_FOR_THIS_*` words, `covers` what the rule matches, then whether it holds in
/// every session or this one, and — for commands that run project scripts — that it allows
/// whatever those scripts do.
///
/// # Example
///
/// ```rust
/// use adk_surface::words;
///
/// assert_eq!(
///     words::always_allow_covering(words::ALWAYS_ALLOW_FOR_THIS_COMMAND, "git push …", true, false),
///     "Always allow for this command (git push …, every session)"
/// );
/// assert_eq!(
///     words::always_allow_covering(words::ALWAYS_ALLOW_FOR_THIS_COMMAND, "npm run …", true, true),
///     "Always allow for this command (npm run …, every session; it runs project scripts, so it allows whatever they do)"
/// );
/// ```
pub fn always_allow_covering(
    label: &str,
    covers: &str,
    every_session: bool,
    runs_scripts: bool,
) -> String {
    let scope = if every_session { "every session" } else { "this session" };
    let scripts =
        if runs_scripts { "; it runs project scripts, so it allows whatever they do" } else { "" };
    format!("{label} ({covers}, {scope}{scripts})")
}
/// Changes the arguments, then allows.
pub const EDIT: &str = "Edit";
/// Refuses.
pub const DENY: &str = "Deny";
/// Approves a high-risk call.
pub const CONFIRM_ITS_YOU_AND_APPROVE: &str = "Confirm it's you and approve";
/// Types a secret yourself.
pub const TAKE_OVER: &str = "Take over";
/// Ends a take-over.
pub const TAKE_OVER_DONE: &str = "Done";
/// Stops every agent.
pub const STOP_ALL: &str = "Stop all";
/// Restarts after stopping; web home only.
pub const RESUME_ALL: &str = "Resume all";
/// Undoes an overnight memory change.
pub const UNDO: &str = "Undo";
/// Confirms a guess.
pub const THATS_RIGHT: &str = "That's right";
/// Corrects a guess.
pub const FIX_IT: &str = "Fix it";
/// Forgets a guess.
pub const FORGET_IT: &str = "Forget it";
/// Where a surface that may not decide sends the person (spec 3.3 §5.3, §7.2).
pub const OPEN_IN_HOME: &str = "Open in home";

// ---------------------------------------------------------------------------
// §7.1 The parts of a decision
// ---------------------------------------------------------------------------

/// A routine call.
pub const LOW_RISK: &str = "Low risk";
/// A call that needs a look.
pub const MEDIUM_RISK: &str = "Medium risk";
/// A call that needs a careful look.
pub const HIGH_RISK: &str = "High risk";
/// A message that, once sent, stays sent.
pub const CANT_BE_UNSENT: &str = "Can't be unsent";
/// Anything else that cannot be taken back.
pub const CANT_BE_UNDONE: &str = "Can't be undone";
/// A timeline step: the decision waited while quiet time held notifications back.
pub const WAITED_THROUGH_QUIET_TIME: &str = "Waited through quiet time";
/// What a screen says when it shows a decision another screen may answer first.
pub const ANSWERED_ELSEWHERE_UPDATES: &str =
    "If you answer somewhere else first, this screen updates.";
/// Why a surface shows no approval for a high-risk or money call.
pub const NEEDS_A_SCREEN: &str =
    "This always needs you, on a screen. Voice, chat and standing permissions can't say yes to it.";
/// Where a high-risk or money call is approved, on a screen that cannot approve it: the tray,
/// or a terminal that is not on the kernel's own computer (spec 3.3 §7.2).
pub const APPROVE_IN_THE_HOME: &str =
    "Approve it in the home app, or in a terminal on the computer your agents run on.";

/// How long a decision has waited, `"since 18:00"`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::since("18:00"), "since 18:00");
/// ```
pub fn since(hh_mm: &str) -> String {
    format!("since {hh_mm}")
}

/// The first timeline step, `"Asked at 18:00"`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::asked_at("18:00"), "Asked at 18:00");
/// ```
pub fn asked_at(hh_mm: &str) -> String {
    format!("Asked at {hh_mm}")
}

/// Where else a decision is shown, `"Also on your phone and in Telegram"`; `None` when it is
/// shown nowhere else.
///
/// # Example
///
/// ```rust
/// use adk_surface::words;
///
/// let places = ["on your phone".to_string(), "in Telegram".to_string()];
/// assert_eq!(words::also_shown(&places).unwrap(), "Also on your phone and in Telegram");
/// assert_eq!(words::also_shown(&[]), None);
/// ```
pub fn also_shown(places: &[String]) -> Option<String> {
    match places {
        [] => None,
        [one] => Some(format!("Also {one}")),
        [rest @ .., last] => Some(format!("Also {} and {last}", rest.join(", "))),
    }
}

/// The answer a decision got, as [`said`] words it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaidWhat {
    /// Allowed the call.
    Yes,
    /// Refused the call.
    No,
    /// Allowed the call with changed arguments.
    Edited,
    /// Answered with a message instead of running the call.
    Replied,
}

/// Who answered a decision, how and when, `"You said yes in Telegram at 07:31"`.
///
/// `who` is `None` for the person looking, `"You"`; `via` is a phrase such as
/// `"in Telegram"` or `"on your phone"`.
///
/// # Example
///
/// ```rust
/// use adk_surface::words::{SaidWhat, said};
///
/// assert_eq!(said(None, SaidWhat::Yes, Some("in Telegram"), "07:31"), "You said yes in Telegram at 07:31");
/// assert_eq!(said(Some("ana"), SaidWhat::No, None, "07:31"), "ana said no at 07:31");
/// ```
pub fn said(who: Option<&str>, what: SaidWhat, via: Option<&str>, hh_mm: &str) -> String {
    let who = who.unwrap_or("You");
    let what = match what {
        SaidWhat::Yes => "said yes",
        SaidWhat::No => "said no",
        SaidWhat::Edited => "said yes with changes",
        SaidWhat::Replied => "answered instead",
    };
    match via {
        Some(via) => format!("{who} {what} {via} at {hh_mm}"),
        None => format!("{who} {what} at {hh_mm}"),
    }
}

/// A decision that ended without an answer from a person: cancelled, or expired under a
/// policy that set an expiry (spec 3.3 §7.3: decisions routed by a surface never expire).
pub const ENDED_WITHOUT_AN_ANSWER: &str = "Ended without an answer";

// ---------------------------------------------------------------------------
// Where an answer came from (spec 3.3 §11)
// ---------------------------------------------------------------------------

/// The web home, as a place: `"in the home app"`.
pub const IN_THE_HOME_APP: &str = "in the home app";
/// The phone, as a place: `"on your phone"`.
pub const ON_YOUR_PHONE: &str = "on your phone";
/// The terminal, as a place: `"in the terminal"`.
pub const IN_THE_TERMINAL: &str = "in the terminal";
/// The desktop tray, as a place: `"from the tray"`.
pub const FROM_THE_TRAY: &str = "from the tray";
/// Voice, as a way of answering: `"by voice"`.
pub const BY_VOICE: &str = "by voice";

/// A chat channel, as a place: `"in Telegram"`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::in_channel("Telegram"), "in Telegram");
/// ```
pub fn in_channel(channel: &str) -> String {
    format!("in {channel}")
}

// ---------------------------------------------------------------------------
// The status line (spec 3.3 §5.1, §6)
// ---------------------------------------------------------------------------

/// Every agent is stopped until the person resumes them from the home.
pub const ALL_AGENTS_STOPPED: &str = "All agents stopped";
/// What chat answers after "stop all" (spec 3.3 §5.3).
pub const ALL_AGENTS_STOPPED_RESUME_IN_HOME: &str =
    "All agents stopped. Use Resume all in the home app.";

/// Spend against the daily limit, `"$1.84 of $5.00 today"`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::spent_of(1_840_000, 5_000_000), "$1.84 of $5.00 today");
/// ```
pub fn spent_of(spent_micro_usd: u64, limit_micro_usd: u64) -> String {
    format!("{} of {} today", dollars(spent_micro_usd), dollars(limit_micro_usd))
}

/// Spend with no daily limit set, `"$0.62 today"`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::spent_today(620_000), "$0.62 today");
/// ```
pub fn spent_today(spent_micro_usd: u64) -> String {
    format!("{} today", dollars(spent_micro_usd))
}

/// Micro-USD as dollars and cents, `"$1.84"`, rounded to the nearest cent.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::dollars(1_845_000), "$1.85");
/// assert_eq!(adk_surface::words::dollars(0), "$0.00");
/// ```
pub fn dollars(micro_usd: u64) -> String {
    let cents = micro_usd.saturating_add(5_000) / 10_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// Quiet time ahead, `"Quiet from 22:00"`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::quiet_from("22:00"), "Quiet from 22:00");
/// ```
pub fn quiet_from(hh_mm: &str) -> String {
    format!("Quiet from {hh_mm}")
}

/// Quiet time now, `"Quiet until 07:00"`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::quiet_until("07:00"), "Quiet until 07:00");
/// ```
pub fn quiet_until(hh_mm: &str) -> String {
    format!("Quiet until {hh_mm}")
}

/// Input typed while a decision is open waits for it (spec 3.3 §5.2, §7.4 rule 2).
pub const SENT_AFTER_THIS_CARD_IS_ANSWERED: &str = "Sent after this card is answered";
/// Input typed while a turn runs, or behind earlier queued input, with no decision open: it
/// is the session's next message (spec 3.3 §5.2, S-I4).
pub const SENT_WHEN_YOUR_AGENT_IS_FREE: &str = "Sent when your agent is free";

/// A spoken "allow once" inside its cancel window, `"Allowing once in 5 seconds"`
/// (spec 3.3 §5.6).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::allowing_in(5), "Allowing once in 5 seconds");
/// ```
pub fn allowing_in(seconds: u64) -> String {
    match seconds {
        1 => "Allowing once in 1 second".to_string(),
        n => format!("Allowing once in {n} seconds"),
    }
}

// ---------------------------------------------------------------------------
// The briefing (spec 3.3 §5.1 Today)
// ---------------------------------------------------------------------------

/// Since when the briefing counts, `"Since 22:04"`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::briefing_since("22:04"), "Since 22:04");
/// ```
pub fn briefing_since(hh_mm: &str) -> String {
    format!("Since {hh_mm}")
}

/// The briefing pill of a decision a person made.
pub const YOU_DECIDED: &str = "You decided";

/// Words a briefing line uses for one ledger row; `agent` is the agent's display name.
///
/// The ledger is content-free (spec 3.2 §7.10), so these lines say what kind of thing
/// happened and link to its record; they never guess at content.
///
/// # Example
///
/// ```rust
/// use adk_surface::words;
///
/// assert_eq!(words::briefing_sentence("held", "Finance"), "Finance asked you first.");
/// assert_eq!(words::briefing_sentence("approval", "Finance"), "You allowed Finance to go ahead.");
/// ```
pub fn briefing_sentence(kind: &str, agent: &str) -> String {
    match kind {
        "intent" => format!("{agent} started on your message."),
        "tool_call" => format!("{agent} used a tool."),
        "approval" => format!("You allowed {agent} to go ahead."),
        "rejection" => format!("You said no to {agent}."),
        "denial" => format!("{agent} was not allowed to do something."),
        "held" => format!("{agent} asked you first."),
        "decided" => format!("A decision for {agent} was made."),
        "rule_hit" => format!("{agent} went ahead, as you allowed without asking."),
        "observation" => IS_THAT_RIGHT.to_string(),
        "wake" => format!("{agent} woke up."),
        "proposal" => {
            format!("{agent} suggests something. Nothing happens until you say so.")
        }
        "spend" => format!("{agent} spent money on a model."),
        "turn" => format!("{agent} finished."),
        "loop_guard" => format!("{agent} stopped because it was repeating itself."),
        "governance" => ALL_AGENTS_STOPPED.to_string(),
        _ => format!("{agent} did something."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fixed word: sentence case, no all-caps labels (spec 3.3 §8).
    const ALL: &[&str] = &[
        WORKING,
        WAITING_FOR_YOU,
        WAITING_FOR_AN_ANSWER,
        ASLEEP,
        DONE,
        FAILED,
        DECISIONS,
        DECIDED,
        QUESTION,
        IS_THAT_RIGHT,
        SUGGESTION,
        DO_IT,
        NOT_NOW,
        WHILE_YOU_WERE_AWAY,
        WHAT_YOUR_AGENTS_DID,
        EVERY_ACTION_ON_RECORD,
        WHAT_YOUR_AGENTS_REMEMBER,
        YOU_SAID_THIS,
        A_GUESS,
        JUST_LOOK,
        SUGGEST,
        ASK_FIRST,
        GO_AHEAD,
        ALLOWED_WITHOUT_ASKING,
        EXTEND,
        STOP_ALLOWING,
        QUIET_TIME,
        DAILY_SPENDING_LIMIT,
        AREAS,
        WHERE_YOU_CAN_SAY_YES_FROM,
        ADD_A_PHONE_OR_APP,
        YOUR_AGENTS,
        AGENTS_SUGGEST,
        NOTICED_SOMETHING,
        ALL_AGENTS_RESUMED,
        ALLOW_ONCE,
        ALWAYS_ALLOW_FOR_THIS_PERSON,
        ALWAYS_ALLOW_FOR_THIS_FOLDER,
        ALWAYS_ALLOW_FOR_THIS_COMMAND,
        ALWAYS_ALLOW_FOR_THIS_SITE,
        EDIT,
        DENY,
        CONFIRM_ITS_YOU_AND_APPROVE,
        TAKE_OVER,
        TAKE_OVER_DONE,
        STOP_ALL,
        RESUME_ALL,
        UNDO,
        THATS_RIGHT,
        FIX_IT,
        FORGET_IT,
        OPEN_IN_HOME,
        LOW_RISK,
        MEDIUM_RISK,
        HIGH_RISK,
        CANT_BE_UNSENT,
        CANT_BE_UNDONE,
        WAITED_THROUGH_QUIET_TIME,
        ANSWERED_ELSEWHERE_UPDATES,
        NEEDS_A_SCREEN,
        APPROVE_IN_THE_HOME,
        ENDED_WITHOUT_AN_ANSWER,
        SENT_AFTER_THIS_CARD_IS_ANSWERED,
        SENT_WHEN_YOUR_AGENT_IS_FREE,
        YOU_SUGGESTED_THIS,
        ALL_AGENTS_STOPPED,
        ALL_AGENTS_STOPPED_RESUME_IN_HOME,
        YOU_DECIDED,
    ];

    #[test]
    fn every_word_is_sentence_case() {
        for word in ALL {
            let first = word.chars().next().unwrap();
            assert!(first.is_uppercase(), "{word:?} starts lower case");
            for token in word.split_whitespace() {
                let letters: Vec<char> = token.chars().filter(|c| c.is_alphabetic()).collect();
                assert!(
                    letters.len() < 2 || letters.iter().any(|c| c.is_lowercase()),
                    "{word:?} has an all-caps word"
                );
            }
        }
    }

    #[test]
    fn the_spec_terms_are_exact() {
        assert_eq!(
            [JUST_LOOK, SUGGEST, ASK_FIRST, GO_AHEAD],
            ["Just look", "Suggest", "Ask first", "Go ahead"]
        );
        assert_eq!(
            said(None, SaidWhat::Yes, Some("in Telegram"), "07:31"),
            "You said yes in Telegram at 07:31"
        );
        assert_eq!(
            also_shown(&[ON_YOUR_PHONE.into(), in_channel("Telegram")]).unwrap(),
            "Also on your phone and in Telegram"
        );
        assert_eq!(spent_of(1_840_000, 5_000_000), "$1.84 of $5.00 today");
    }
}
