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
/// Sends the person's answer to what an MCP server asks (an elicitation form).
pub const ANSWER: &str = "Answer";
/// Over an elicitation address whose host has look-alike characters.
pub const LOOK_ALIKE_ADDRESS: &str =
    "This address uses look-alike characters. Check the host letter by letter before you open it.";
/// Over an elicitation address that hides a name before an `@`.
pub const HIDDEN_HOST_ADDRESS: &str =
    "This address puts a name before an @, so the real host is the part after it.";
/// Over an elicitation address that is not encrypted.
pub const UNENCRYPTED_ADDRESS: &str = "This address is not encrypted (http).";
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

// The phone (spec 3.3 §5.4, §8.2).

/// The only words a wake-up shows (spec 3.3 §5.4, S-I5): the notification's title.
pub const A_DECISION_IS_WAITING: &str = "A decision is waiting";
/// The notification's body.
pub const OPEN_THE_APP_TO_SEE_IT: &str = "Open the app to see it";
/// A phone's name in "Where you can say yes from".
pub const YOUR_PHONE: &str = "Your phone";
/// What a phone with a passkey asks before a high-risk approval, after "Your phone · ".
pub const ASKS_FOR_FACE_OR_FINGERPRINT: &str = "asks for your face or fingerprint";
/// What a high-risk card needs on the phone, after its reversibility: "Can't be undone · needs
/// your face or fingerprint".
pub const NEEDS_FACE_OR_FINGERPRINT: &str = "needs your face or fingerprint";
/// How an "allow once" answer reads on a decided card: "You said yes in Telegram at 07:31 ·
/// done once".
pub const DONE_ONCE: &str = "done once";
/// What the decide screen says after the agent's name.
pub const WANTS_TO_RUN_THIS_COMMAND: &str = "wants to run this command.";
/// Sets up the phone's or the computer's passkey.
pub const USE_YOUR_FACE_OR_FINGERPRINT: &str = "Use your face or fingerprint";
/// Sets up the computer's passkey for the web home.
pub const USE_YOUR_FACE_OR_FINGERPRINT_ON_THIS_COMPUTER: &str =
    "Use your face or fingerprint on this computer";
/// Asks the phone for permission to wake it.
pub const ALLOW_NOTIFICATIONS: &str = "Allow notifications";
/// What Safari on iOS shows before the app is installed: notifications and the app's own
/// storage need the Home Screen app.
pub const ADD_TO_HOME_SCREEN_FIRST: &str = "Add to Home Screen first";
/// Where a high-risk call goes when this phone cannot confirm it is you.
pub const APPROVE_IT_IN_THE_HOME: &str = "Approve it in the home on your computer";

/// What the decide screen says: "Code wants to run this command."
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::wants_to_run("Code"), "Code wants to run this command.");
/// ```
pub fn wants_to_run(agent: &str) -> String {
    format!("{agent} {WANTS_TO_RUN_THIS_COMMAND}")
}

// ---------------------------------------------------------------------------
// Voice (spec 3.3 §5.6)
// ---------------------------------------------------------------------------

/// The voice view while the model waits for the person.
pub const LISTENING: &str = "Listening";
/// The voice view while the model speaks.
pub const SPEAKING: &str = "Speaking";
/// What to say to a prompted decision voice may allow once.
pub const SAY_ALLOW_ONCE_OR_DENY: &str = "Say \"allow once\" or \"deny\".";
/// What to say to a prompted decision voice cannot approve.
pub const SAY_DENY_OR_DECIDE_ON_SCREEN: &str = "Say \"deny\", or decide on screen.";
/// Where the voice view sends a decision voice cannot approve.
pub const DECIDE_ON_SCREEN: &str = "Decide on screen";
/// Cancels a spoken "allow once" inside its cancel window.
pub const CANCEL: &str = "Cancel";
/// Ends a voice conversation.
pub const END: &str = "End";
/// The voice view's card of the camera and the screen.
pub const WHAT_IT_CAN_SEE: &str = "What it can see";
/// What voice may decide, and what is kept of it.
pub const VOICE_PRIVACY_NOTE: &str = "By voice you can say yes once to everyday things, and no \
     to anything. Serious ones need a screen to approve. We keep a note of your answer, never \
     the recording.";
/// A spoken "allow once" cancelled inside its window.
pub const SPOKEN_ANSWER_CANCELLED: &str = "Cancelled. It still waits for you.";
/// Nothing was decided: the prompted decision changed while the person spoke.
pub const CHANGED_WHILE_YOU_SPOKE: &str =
    "Nothing was decided: the decision changed while you were speaking.";
/// A spoken approval of a call voice cannot approve.
pub const VOICE_CANT_APPROVE_THIS: &str = "Voice can't approve this one. Decide on screen.";
/// A spoken "stop all" from someone who may not stop every agent.
pub const STOPPING_NEEDS_THE_OPERATOR: &str =
    "Stopping every agent needs the operator. Ask them, or stop from their home app.";

/// The voice view's header, `"Talking with Chief of staff"`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::talking_with("Chief of staff"), "Talking with Chief of staff");
/// ```
pub fn talking_with(agent: &str) -> String {
    format!("Talking with {agent}")
}

/// What the person said, as transcribed, `"Heard \"allow once\""`.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::heard("allow once"), "Heard \"allow once\"");
/// ```
pub fn heard(text: &str) -> String {
    format!("Heard \"{text}\"")
}

/// A prompted decision, `"Finance wants to send the October invoice to [CLIENT]."`.
///
/// # Example
///
/// ```rust
/// assert_eq!(
///     adk_surface::words::wants_to("Finance", "Send the October invoice to [CLIENT]"),
///     "Finance wants to send the October invoice to [CLIENT]."
/// );
/// ```
pub fn wants_to(agent: &str, heading: &str) -> String {
    let mut chars = heading.chars();
    let heading = match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    };
    format!("{agent} wants to {heading}.")
}

/// Why voice cannot approve a decision, `"\"Publish the release\" is high risk, so voice can't
/// approve it."`; `money` for a call that spends money.
///
/// # Example
///
/// ```rust
/// use adk_surface::words;
///
/// assert_eq!(
///     words::voice_cant_approve("Publish the release", false),
///     "\"Publish the release\" is high risk, so voice can't approve it."
/// );
/// assert_eq!(
///     words::voice_cant_approve("Pay the invoice", true),
///     "\"Pay the invoice\" spends money, so voice can't approve it."
/// );
/// ```
pub fn voice_cant_approve(heading: &str, money: bool) -> String {
    let why = if money { "spends money" } else { "is high risk" };
    format!("\"{heading}\" {why}, so voice can't approve it.")
}

// ---------------------------------------------------------------------------
// §5.7 Your agents, §8.1–§8.2 rows
// ---------------------------------------------------------------------------

/// Starts adding an agent.
pub const NEW_AGENT: &str = "New agent";
/// Ask an agent to suggest one.
pub const DESCRIBE_IT: &str = "Describe it";
/// Fill in the definition.
pub const SET_IT_UP_YOURSELF: &str = "Set it up yourself";
/// Starts a thread with the primary agent holding the request.
pub const ASK_TO_SUGGEST_ONE: &str = "Ask ADK-Rust to suggest one";
/// Approves a suggested agent.
pub const ADD_THIS_AGENT: &str = "Add this agent";
/// Approves a high-risk suggested agent after confirming presence.
pub const CONFIRM_ITS_YOU_AND_ADD: &str = "Confirm it's you and add";
/// Opens the form with the suggestion.
pub const CHANGE_IT_FIRST: &str = "Change it first";
/// Refuses a suggested agent.
pub const DONT_ADD: &str = "Don't add";
/// Saves an agent's settings.
pub const SAVE_CHANGES: &str = "Save changes";
/// Gives an agent one of the person's rules.
pub const COPY_ONE_OF_YOUR_RULES: &str = "Copy one of your rules";
/// Pauses one agent.
pub const PAUSE: &str = "Pause";
/// Resumes one agent.
pub const RESUME: &str = "Resume";
/// Removes one agent, and confirms the removal.
pub const REMOVE: &str = "Remove";
/// Keeps the agent instead of removing it.
pub const KEEP: &str = "Keep";
/// An agent the person added that has not run yet.
pub const READY: &str = "Ready";
/// An agent paused by hand.
pub const PAUSED: &str = "Paused";
/// An agent the kernel cannot build.
pub const CANT_START: &str = "Can't start";
/// The reversibility of adding an agent.
pub const YOU_CAN_REMOVE_IT_LATER: &str = "You can remove it later";
/// The reversibility of a change a person can make again the other way, which a feature's
/// card hint says (spec 3.3 §7.1).
pub const YOU_CAN_CHANGE_IT_BACK: &str = "You can change it back";
/// Over what the home lets every agent on it do, under "Here is what Finance can do"
/// (spec 3.3 §5.7).
pub const ON_THIS_HOME_IT_CAN_ALSO: &str = "On this home it can also:";
/// What a new agent may do without asking.
pub const NOTHING_YET: &str = "Nothing yet";
/// The roster with no agent of the person's own.
pub const NO_AGENTS_OF_YOUR_OWN: &str =
    "No agents of your own yet. Add one, or ask ADK-Rust to suggest one.";
/// The box of **Describe it**.
pub const WHAT_SHOULD_IT_DO: &str = "What should it do?";
/// What **Describe it** puts before the person's words in the message to the primary agent.
pub const SUGGEST_A_NEW_AGENT_FOR: &str = "Suggest a new agent for this: ";

/// The heading of the decision that adds an agent: "Add an agent called Finance".
pub fn add_an_agent_called(name: &str) -> String {
    format!("Add an agent called {name}")
}

/// "Here is what Finance can do".
pub fn here_is_what_can_do(name: &str) -> String {
    format!("Here is what {name} can do")
}

/// The button that adds an agent set up by hand: "Add Finance".
pub fn add_named(name: &str) -> String {
    format!("Add {name}")
}

/// How an agent came to be added: "Suggested by ADK-Rust" (spec 3.3 §8.1).
pub fn suggested_agent_by(agent: &str) -> String {
    format!("Suggested by {agent}")
}

/// The reason a decision ended when its agent was removed: "You removed Finance at 10:02".
pub fn removed_at(name: &str, hh_mm: &str) -> String {
    format!("You removed {name} at {hh_mm}")
}

/// What a message to a paused agent gets: "Finance is paused".
pub fn is_paused(name: &str) -> String {
    format!("{name} is paused")
}

/// The mode cap in an added agent's own thread: "Finance asks first at most. Change it in
/// Finance's settings."
pub fn asks_at_most(name: &str, mode: &str) -> String {
    format!("{name} {} at most. Change it in {name}'s settings.", mode_status(mode).to_lowercase())
}

fn plural(count: u32, one: &str, many: &str) -> String {
    if count == 1 { format!("1 {one}") } else { format!("{count} {many}") }
}

/// What removing an agent ends: "Removing Finance stops 1 piece of work, ends 2 decisions
/// waiting for you and 1 routine. Its threads stay."
pub fn removing_ends(name: &str, ended: &crate::agents::EndedWork) -> String {
    let mut ends = Vec::new();
    if ended.decisions > 0 {
        ends.push(format!("{} waiting for you", plural(ended.decisions, "decision", "decisions")));
    }
    if ended.routines > 0 {
        ends.push(plural(ended.routines, "routine", "routines"));
    }
    if ended.follow_ups > 0 {
        ends.push(plural(ended.follow_ups, "follow-up", "follow-ups"));
    }
    let ends = match ends.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    let body = match (ended.work, ends.is_empty()) {
        (0, true) => format!("Removing {name} ends nothing that is running"),
        (0, false) => format!("Removing {name} ends {ends}"),
        (work, true) => {
            format!("Removing {name} stops {}", plural(work, "piece of work", "pieces of work"))
        }
        (work, false) => format!(
            "Removing {name} stops {}, ends {ends}",
            plural(work, "piece of work", "pieces of work")
        ),
    };
    format!("{body}. Its threads stay.")
}

/// A suggestion the kernel refused when it was approved: "Finance wasn't added: ADK-Rust can
/// no longer use the web, so Finance can't either."
pub fn was_not_added(name: &str, reason: &str) -> String {
    format!("{name} wasn't added: {reason}")
}

/// The comparison line on a suggested agent's card: "It can do no more than ADK-Rust, and
/// has no shell or web." `none` lists what it has none of: `files`, `shell`, `web`.
///
/// # Example
///
/// ```rust
/// use adk_surface::words;
///
/// assert_eq!(words::no_more_than("ADK-Rust", &[]), "It can do no more than ADK-Rust.");
/// assert_eq!(
///     words::no_more_than("ADK-Rust", &["files", "shell", "web"]),
///     "It can do no more than ADK-Rust, and has no files, shell or web."
/// );
/// ```
pub fn no_more_than(suggester: &str, none: &[&str]) -> String {
    match none {
        [] => format!("It can do no more than {suggester}."),
        [one] => format!("It can do no more than {suggester}, and has no {one}."),
        [rest @ .., last] => {
            format!("It can do no more than {suggester}, and has no {} or {last}.", rest.join(", "))
        }
    }
}

/// The roster's entry for the person's removed agents, whose threads stay readable.
pub const REMOVED_AGENTS: &str = "Removed agents";

/// When an agent was removed: "Removed on 25 Oct".
pub fn removed_on(day: &str) -> String {
    format!("Removed on {day}")
}

/// What a removed agent's thread says where the message box was: "Finance was removed. Its
/// threads stay; nothing more runs."
pub fn was_removed(name: &str) -> String {
    format!("{name} was removed. Its threads stay; nothing more runs.")
}

/// A change refused because someone changed the agent first: "Finance was changed in the
/// terminal at 10:01".
pub fn changed_in(name: &str, place: &str, hh_mm: &str) -> String {
    format!("{name} was changed {place} at {hh_mm}")
}

/// The record's sentence of a change: "You added Finance in the home app".
pub fn agent_change(kind: &str, name: &str, place: &str) -> String {
    match kind {
        "proposed" => format!("{name} was suggested {place}"),
        "created" => format!("You added {name} {place}"),
        "paused" => format!("You paused {name} {place}"),
        "resumed" => format!("You resumed {name} {place}"),
        "removing" | "removed" => format!("You removed {name} {place}"),
        "refused" => format!("{name} wasn't added"),
        _ => format!("You changed {name} {place}"),
    }
}

// ---------------------------------------------------------------------------
// Agent-drawn UI (spec 3.3 §6.1 `genui`, §7.4 rule 5)
// ---------------------------------------------------------------------------

/// A drawn form edited and not yet sent.
pub const NOT_SENT_YET: &str = "Not sent yet";
/// The folded earlier versions of a drawing, before their count: "Earlier versions · 3".
pub const EARLIER_VERSIONS: &str = "Earlier versions";
/// A drawn form's button while its send is on the way.
pub const SENDING: &str = "Sending…";
/// The message box's button that picks images to send (spec 3.3 §5.1).
pub const ATTACH_IMAGES: &str = "Attach images";
/// The images waiting in the message box.
pub const IMAGES_TO_SEND: &str = "Images to send";
/// The button on an image in the message box that takes it out.
pub const REMOVE_IMAGE: &str = "Remove image";
/// An image of a message, opened large.
pub const OPEN_IMAGE: &str = "Open image";
/// A run's badge when it ran in the sandbox (spec 3.3 §5.1).
pub const SANDBOX: &str = "Sandbox";
/// A run's badge when it ran on this computer, with no sandbox.
pub const ON_THIS_COMPUTER: &str = "On this computer";
/// Under a run whose output the tool cut: the whole of it is kept for the agent.
pub const THE_AGENT_CAN_READ_ALL_OF_IT: &str = "The agent can read all of it.";
/// A run that ran out of time.
pub const TIMED_OUT: &str = "Timed out";
/// A run that ran out of memory.
pub const OUT_OF_MEMORY: &str = "Out of memory";
/// A run the sandbox stopped.
pub const REFUSED_BY_THE_SANDBOX: &str = "Refused by the sandbox";
/// A run that could not start.
pub const COULD_NOT_RUN: &str = "Could not run";
/// The message box's control that stops the session's running turn (spec 3.3 §5.1).
pub const STOP_THE_TURN: &str = "Stop the turn";
/// After a turn was stopped: the agent waits for the next message.
pub const TURN_STOPPED: &str = "Stopped. Your agent waits for your next message.";
/// The replies an agent suggests, under its message (spec 3.3 §5.1).
pub const SUGGESTED_REPLIES: &str = "Suggested replies";
/// Today's section that counts the person's answers (spec 3.3 §5.1).
pub const HOW_YOU_ANSWERED: &str = "How you answered this week";
/// A drawn form sent and denied.
pub const SENT_YOU_SAID_NO: &str = "Sent · you said no";
/// A drawn form sent, allowed, and its call failed.
pub const SENT_FAILED: &str = "Sent · failed";
/// A drawn form sent at a version the agent had changed since (`surface.drawn_changed`).
pub const FORM_CHANGED_SEND_AGAIN: &str =
    "Couldn't send: this form changed — check it and send again";
/// Under a drawn form's buttons in the terminal and in the scrollback.
pub const SENDING_ASKS_YOU_FIRST: &str = "Sending asks you first";
/// What the home lets every agent on it do with `genui`, under "On this home it can also:"
/// (spec 3.3 §5.7, wave 4 decision 14).
pub const SHOW_FORMS_AND_TABLES: &str =
    "Show forms and tables in your threads — sending asks you first";
/// A drawn form sent by voice, from chat or by an agent: only a person at a screen sends one.
pub const SEND_FORMS_FROM_A_SCREEN: &str =
    "A form an agent drew is sent from a screen, by the person who filled it in";
/// A drawn form sent while its thread's agent is working or input waits for it.
pub const SEND_FORM_WHEN_FREE: &str =
    "Couldn't send: your agent is still working in this thread — send it when it's done";
/// A drawn form sent again before its first send reached the agent.
pub const FORM_BEING_SENT: &str = "Couldn't send: this form is being sent already";

/// A 3D scene's toggle: its words instead of the scene.
pub const SHOW_AS_TEXT: &str = "Show as text";
/// Opens a chart's numbers as a table, for a screen reader or a closer look.
pub const SHOW_THE_DATA: &str = "Show the data";
/// A 3D scene's toggle back from its words.
pub const SHOW_IN_3D: &str = "Show in 3D";
/// Stops a scene's motion until it is played again.
pub const PAUSE_MOTION: &str = "Pause motion";
/// Plays a scene's motion again.
pub const PLAY_MOTION: &str = "Play motion";
/// Turns a flip card to its back.
pub const SHOW_BACK: &str = "Show back";
/// Turns a flip card to its front.
pub const SHOW_FRONT: &str = "Show front";
/// A flip card's front, in the terminal.
pub const FRONT: &str = "Front";
/// A flip card's back, in the terminal.
pub const BACK: &str = "Back";
/// Brings the next item of a carousel to the front.
pub const NEXT: &str = "Next";
/// Brings the previous item of a carousel to the front.
pub const PREVIOUS: &str = "Previous";
/// A scene object in the `ok` state.
pub const STATE_OK: &str = "OK";
/// A scene object in the `warning` state.
pub const STATE_WARNING: &str = "Warning";
/// A scene object in the `critical` state.
pub const STATE_CRITICAL: &str = "Critical";
/// A scene object in the `info` state.
pub const STATE_INFO: &str = "Info";
/// Where WebGL is missing or failed: the scene is shown in words.
pub const SCENE_SHOWN_AS_TEXT: &str = "3D isn't available here, so this is shown as text";

/// Under a scene whose objects can be picked: "Picking one fills Dock. Nothing is sent."
pub fn scene_picks(field: &str) -> String {
    format!("Picking one fills {field}. Nothing is sent.")
}

/// Said when a scene's object is picked: "Picked: Dock 3".
pub fn picked(label: &str) -> String {
    format!("Picked: {label}")
}

/// What a 3D scene shows, in one sentence for screen readers and the terminal: "3D bar chart
/// of Revenue, 24 bars, from 3 to 98", "3D scene of Dock 1, Dock 2 and 4 more, 12 objects".
pub fn scene_summary(
    kind: &str,
    names: &[&str],
    counted: &str,
    range: Option<(f64, f64)>,
) -> String {
    let named: Vec<&str> = names.iter().copied().filter(|name| !name.trim().is_empty()).collect();
    let of = match named.as_slice() {
        [] => String::new(),
        [one] => format!(" of {one}"),
        [rest @ .., last] => format!(" of {} and {last}", rest.join(", ")),
    };
    match range {
        Some((low, high)) => {
            format!("{kind}{of}, {counted}, from {} to {}", number(low), number(high))
        }
        None => format!("{kind}{of}, {counted}"),
    }
}

/// `count` of a thing: "1 bar", "24 bars".
pub fn counted(count: usize, one: &str, many: &str) -> String {
    if count == 1 { format!("1 {one}") } else { format!("{count} {many}") }
}

/// Over a drawing: "Drawn by Finance · 10:02".
pub fn drawn_by(agent: &str, hh_mm: &str) -> String {
    format!("Drawn by {agent} · {hh_mm}")
}

/// Under a drawn form's button: "Sending runs send_invoice and asks you first".
pub fn sending_runs(tool: &str) -> String {
    format!("Sending runs {tool} and asks you first")
}

/// What a drawn chart shows, in one sentence for screen readers and the terminal: "Line
/// chart of Revenue and Costs, 12 points, from 3100 to 9800".
pub fn chart_summary(
    kind: &str,
    names: &[&str],
    points: usize,
    range: Option<(f64, f64)>,
) -> String {
    let named: Vec<&str> = names.iter().copied().filter(|name| !name.trim().is_empty()).collect();
    let of = match named.as_slice() {
        [] => String::new(),
        [one] => format!(" of {one}"),
        [rest @ .., last] => format!(" of {} and {last}", rest.join(", ")),
    };
    let count = if points == 1 { "1 point".to_string() } else { format!("{points} points") };
    match range {
        Some((low, high)) if points > 0 => {
            format!("{kind} chart{of}, {count}, from {} to {}", number(low), number(high))
        }
        _ => format!("{kind} chart{of}, {count}"),
    }
}

/// `value` as a person reads it in a chart's summary: no more than two decimals.
fn number(value: f64) -> String {
    let rounded = (value * 100.0).round() / 100.0;
    if rounded.fract() == 0.0 { format!("{rounded:.0}") } else { format!("{rounded}") }
}

/// A drawing refused because it looks like a decision.
pub fn looked_like_a_decision(agent: &str) -> String {
    format!("{agent} tried to show something that looks like a decision. It wasn't shown.")
}

/// A drawing refused because the home's catalog cannot show it.
pub fn cant_show_here(agent: &str) -> String {
    format!("{agent} tried to show something this home can't show.")
}

/// A drawing refused because it asks for a password or another secret.
pub fn asked_for_a_password(agent: &str) -> String {
    format!("{agent} asked for a password in a form. Use Take over for passwords.")
}

/// A drawn form sent, before its call is held: "Sent at 10:03".
pub fn sent_at(hh_mm: &str) -> String {
    format!("Sent at {hh_mm}")
}

/// A drawn form sent and waiting: "Sent at 10:03 · waiting for you".
pub fn sent_waiting(hh_mm: &str) -> String {
    format!("Sent at {hh_mm} · waiting for you")
}

/// A drawn form sent, allowed and run: "Sent · done at 10:04".
pub fn sent_done(hh_mm: &str) -> String {
    format!("Sent · done at {hh_mm}")
}

/// A drawn form whose call the drawing agent's rules refuse: "Finance can't use send_invoice
/// here".
pub fn cant_use_here(agent: &str, tool: &str) -> String {
    format!("{agent} can't use {tool} here")
}

/// A drawn form's call allowed, then refused when it resumed because a rule changed.
pub fn cant_use_any_more(agent: &str, tool: &str) -> String {
    format!("Not run: {agent} can't use {tool} any more")
}

/// Over the newer version of a drawn form: "Finance changed this form at 10:05".
pub fn changed_this_form(agent: &str, hh_mm: &str) -> String {
    format!("{agent} changed this form at {hh_mm}")
}

/// Over the newer version of a drawn card or table: "Finance changed this at 10:05".
pub fn changed_this(agent: &str, hh_mm: &str) -> String {
    format!("{agent} changed this at {hh_mm}")
}

/// Where a drawing an agent removed was: "Finance removed this at 10:06".
pub fn removed_this(agent: &str, hh_mm: &str) -> String {
    format!("{agent} removed this at {hh_mm}")
}

/// A collapsed older version of a drawing: "Earlier version · 10:02".
pub fn earlier_version(hh_mm: &str) -> String {
    format!("Earlier version · {hh_mm}")
}

/// Under a long drawn table cut short: "Show all 42".
pub fn show_all(count: usize) -> String {
    format!("Show all {count}")
}

/// The first step of a decision a drawn form asked for (spec 3.3 §7.1, wave 4 decision 12):
/// "You pressed Send on a form Finance drew · 10:03".
pub fn pressed_send_on_form(agent: &str, hh_mm: &str) -> String {
    format!("You pressed Send on a form {agent} drew · {hh_mm}")
}

/// The detail line of a decision a drawn form asked for: "From a form Finance drew".
pub fn from_a_form(agent: &str) -> String {
    format!("From a form {agent} drew")
}

// ---------------------------------------------------------------------------
// Apps from MCP servers (spec 3.3 §6.1 `genui-apps`)
// ---------------------------------------------------------------------------

/// An app being fetched and checked before it is framed.
pub const OPENING_THE_APP: &str = "Opening the app…";
/// Where an app is not shown: the phone, a remote terminal, or a home without an apps origin.
pub const OPEN_THE_HOME_TO_SEE_THIS_APP: &str = "Open the home on this computer to see this app";
/// An app whose HTML shows a decision's words.
pub const APP_BLOCKED_APPROVAL_BUTTONS: &str = "This app was blocked: it shows approval buttons";
/// An app that loads or connects to a site it did not declare.
pub const APP_BLOCKED_UNDECLARED_SITES: &str =
    "This app was blocked: it asks for sites it didn't declare";
/// An app that asks to reach this computer, its network or the home.
pub const APP_BLOCKED_THIS_COMPUTER: &str = "This app was blocked: it asks to reach this computer";
/// An app whose resource is not an MCP App's HTML.
pub const APP_BLOCKED_NOT_AN_APP: &str = "This app was blocked: it isn't an app this home can show";
/// An app whose server did not answer.
pub const APP_CANT_BE_OPENED: &str = "This app can't be opened right now";
/// An app asked for a tool only an app may call (wave 4 decision 15).
pub const APP_NEEDS_A_TOOL: &str = "This app needs a tool this home can't run yet";
/// Sends what an app suggested, as the person's message.
pub const SEND_IT: &str = "Send it";
/// Under what an app suggests sending.
pub const MARKED_AS_SUGGESTED_BY_THE_APP: &str = "It will be marked as suggested by the app";
/// Opens a link an app asked to open.
pub const OPEN: &str = "Open";
/// Saves a file an app offered.
pub const SAVE: &str = "Save";
/// What an app's tool call answers the app once it is held: it ran nothing.
pub const APP_CALL_WAITS_FOR_YOU: &str = "Sent to the person. It runs only once they decide.";
/// What an app's tool call answers the app when the agent has not yet held or refused it.
pub const APP_CALL_SENT: &str = "Sent. It runs only if the person allows it.";
/// What an app's tool call answers the app when its server did not list its tools.
pub const APP_SERVER_DIDNT_ANSWER: &str = "The app's server didn't answer. Try again in a moment.";

/// Over an app in a thread: "App from Weather · it can't approve anything".
pub fn app_from(app: &str) -> String {
    format!("App from {app} · it can't approve anything")
}

/// An app in the terminal: "app from Weather · open it in the home".
pub fn app_in_the_terminal(app: &str) -> String {
    format!("app from {app} · open it in the home")
}

/// An app asked for a tool it may not use: "The app tried to use transfer_funds. Apps can't
/// use it."
pub fn app_cant_use(tool: &str) -> String {
    format!("The app tried to use {tool}. Apps can't use it.")
}

/// An app suggests a message: "The app wants to send: “Remind me to take an umbrella”".
pub fn app_wants_to_send(message: &str) -> String {
    format!("The app wants to send: “{message}”")
}

/// An app asks to open a link: "Open example.com?"
pub fn open_site(host: &str) -> String {
    format!("Open {host}?")
}

/// An app offers a file: "The Weather app wants to save forecast.csv (2 KB)".
pub fn app_wants_to_save(app: &str, name: &str, bytes: usize) -> String {
    let size = if bytes < 1024 {
        format!("{bytes} bytes")
    } else if bytes < 1024 * 1024 {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    };
    format!("The {app} app wants to save {name} ({size})")
}

/// An app that already has `count` calls waiting asks for another: "The app already asked for 3
/// things. Answer those first."
pub fn app_asked_enough(count: usize) -> String {
    format!("The app already asked for {count} things. Answer those first.")
}

/// The thread's line for a call an app asked for: "The app asked · 10:06".
pub fn app_asked_at(hh_mm: &str) -> String {
    format!("The app asked · {hh_mm}")
}

/// The first step of a decision an app asked for: "The app from Weather asked · 10:06".
pub fn app_asked(app: &str, hh_mm: &str) -> String {
    format!("The app from {app} asked · {hh_mm}")
}

/// The detail line of a decision an app asked for: "From the Weather app in this thread".
pub fn from_the_app(app: &str) -> String {
    format!("From the {app} app in this thread")
}

/// The label of a message an app suggested and the person sent: "Suggested by the Weather
/// app".
pub fn suggested_by_the_app(app: &str) -> String {
    format!("Suggested by the {app} app")
}

/// Where an agent's commands run (spec 3.3 §5.7): "Commands run in the sandbox (Seatbelt)".
pub fn commands_run(place: &crate::model::ShellPlace) -> String {
    use crate::model::ShellPlace;
    match place {
        ShellPlace::Host => "Commands run on this computer".to_string(),
        ShellPlace::Sandbox { enforcer: Some(enforcer) } => {
            format!("Commands run in the sandbox ({})", enforcer_name(enforcer))
        }
        ShellPlace::Sandbox { enforcer: None } => "Commands run in the sandbox".to_string(),
        ShellPlace::Container { image } => format!("Commands run in a container ({image})"),
        ShellPlace::Remote { provider, off_host: true } => {
            format!("Commands run in a remote sandbox ({provider})")
        }
        ShellPlace::Remote { provider, off_host: false } => {
            format!("Commands run in a sandbox ({provider})")
        }
        ShellPlace::Degraded { reason, .. } => {
            format!("The sandbox is unavailable, so commands run on this computer: {reason}")
        }
    }
}

/// An OS sandbox enforcer's name as people know it.
fn enforcer_name(enforcer: &str) -> &str {
    match enforcer {
        "seatbelt" => "Seatbelt",
        "bubblewrap" => "bubblewrap",
        "landlock" => "Landlock",
        other => other,
    }
}

/// How many images a message carries: "1 image", "3 images".
pub fn images_attached(count: usize) -> String {
    if count == 1 { "1 image".to_string() } else { format!("{count} images") }
}

/// Under a run's output: "Showing the last 40 lines of 214."
pub fn last_lines(shown: usize, total: usize) -> String {
    format!("Showing the last {shown} lines of {total}.")
}

/// How a person answered: "12 answers: 9 allowed once, 2 always allowed, 1 denied".
pub fn answers_line(
    total: u32,
    allowed_once: u32,
    always_allowed: u32,
    edited: u32,
    denied: u32,
) -> String {
    if total == 0 {
        return "No answers this week.".to_string();
    }
    let parts: Vec<String> = [
        (allowed_once, "allowed once"),
        (always_allowed, "always allowed"),
        (edited, "edited"),
        (denied, "denied"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, what)| format!("{count} {what}"))
    .collect();
    let noun = if total == 1 { "answer" } else { "answers" };
    format!("{total} {noun}: {}", parts.join(", "))
}

// ---------------------------------------------------------------------------
// Approve with the image (spec 3.7 §8.2; board choices S3, S4, S5, A4, A12, A13, A15, A20, A21)
// ---------------------------------------------------------------------------

/// A card's single image, waiting.
pub const THE_EXACT_IMAGE_THAT_WOULD_LEAVE: &str = "The exact image that would leave";
/// A card's single image, once it left.
pub const THE_IMAGE_THAT_LEFT: &str = "The image that left";
/// A card's single image, deleted before anything was sent.
pub const THE_IMAGE_THAT_WOULD_HAVE_LEFT: &str = "The image that would have left";
/// Every pixel of the image is covered.
pub const ALL_OF_THE_IMAGE: &str = "100% of the image";
/// An image no region covers.
pub const NOTHING_COVERED: &str = "nothing covered";
/// The lead of the notice on a card whose image is gone.
pub const NOTHING_TO_SEND: &str = "Nothing to send.";
/// The lead of the notice on a card whose image is covered in full.
pub const EVERYTHING_IS_COVERED: &str = "Everything is covered.";
/// An image deleted before its retention ran out.
pub const IMAGE_DELETED: &str = "This image was deleted";
/// Why, when the image went before its retention ran out.
pub const IMAGE_DELETED_EARLY: &str =
    "It was deleted before its time as evidence ran out, with what it was cut from.";
/// The fixed line under every card that holds an image (board choice A21).
pub const IMAGES_LEAVE_ONLY_COVERED: &str = "Images leave this computer only after faces and personal details are covered. Nothing on this card can uncover them.";
/// Why voice cannot say yes to sending an image (board choice A18).
pub const LOOK_AT_THE_IMAGE_ON_A_SCREEN: &str = "To decide, look at the image on a screen. Voice can't say yes to sending an image; \u{201c}Deny\u{201d} still works.";
/// Why a surface that cannot show the image sends the person to the home (board choice A17).
pub const SHOW_THE_IMAGE_IN_THE_HOME: &str =
    "This screen can't show the image. Look at it in the home before you decide.";
/// The old card's line once its image was changed and the call held again.
pub const HELD_AGAIN_WITH_THE_NEW_IMAGE: &str = "held again with the new image";
/// Open the image's source, for a viewer allowed to (board choice A9).
pub const OPEN_THE_SOURCE_IMAGE: &str = "Open the source image";

/// "The 3 images that would leave".
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::the_images_that_would_leave(3), "The 3 images that would leave");
/// ```
pub fn the_images_that_would_leave(count: usize) -> String {
    format!("The {count} images that would leave")
}

/// "The 3 images that left".
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::the_images_that_left(2), "The 2 images that left");
/// ```
pub fn the_images_that_left(count: usize) -> String {
    format!("The {count} images that left")
}

/// "The 3 images that would have left".
///
/// # Example
///
/// ```rust
/// assert_eq!(
///     adk_surface::words::the_images_that_would_have_left(3),
///     "The 3 images that would have left"
/// );
/// ```
pub fn the_images_that_would_have_left(count: usize) -> String {
    format!("The {count} images that would have left")
}

/// An image's size and format: "1012 × 638 PNG".
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::image_size(1012, 638, "image/png"), "1012 × 638 PNG");
/// ```
pub fn image_size(width: u32, height: u32, mime_type: &str) -> String {
    let format = mime_type.strip_prefix("image/").unwrap_or(mime_type).to_ascii_uppercase();
    format!("{width} × {height} {format}")
}

/// A class of covering, counted: "1 face", "3 personal text", "2 fields your schema marks
/// personal", "1 box you covered" (board choices S3, A4).
///
/// # Example
///
/// ```rust
/// use adk_surface::CoverClass;
///
/// assert_eq!(adk_surface::words::covered_class(CoverClass::Face, 1), "1 face");
/// assert_eq!(
///     adk_surface::words::covered_class(CoverClass::SchemaField, 2),
///     "2 fields your schema marks personal"
/// );
/// ```
pub fn covered_class(class: crate::CoverClass, count: u32) -> String {
    let one = count == 1;
    match class {
        crate::CoverClass::Face => format!("{count} face{}", if one { "" } else { "s" }),
        crate::CoverClass::PersonalText => format!("{count} personal text"),
        crate::CoverClass::SchemaField => {
            format!("{count} field{} your schema marks personal", if one { "" } else { "s" })
        }
        crate::CoverClass::Marked => {
            format!("{count} box{} you covered", if one { "" } else { "es" })
        }
        crate::CoverClass::Other => format!("{count} other region{}", if one { "" } else { "s" }),
    }
}

/// A region's tag over a large picture: "face" for a face the redactor found, "face · whole
/// person" for a person covered whole because no face was found (spec 3.7 Q3, board choice A4),
/// "personal text", "schema field", "your redaction", or the feature's own label.
///
/// # Example
///
/// ```rust
/// use adk_surface::CoverClass;
///
/// assert_eq!(adk_surface::words::region_tag(CoverClass::Face, "faces"), "face");
/// assert_eq!(adk_surface::words::region_tag(CoverClass::Face, "people"), "face · whole person");
/// ```
pub fn region_tag(class: crate::CoverClass, label: &str) -> String {
    match class {
        crate::CoverClass::Face => match label {
            "faces" | "face" => "face".to_string(),
            // `person` and `people` cover the whole person; an unknown label keeps the wider tag.
            _ => "face · whole person".to_string(),
        },
        crate::CoverClass::PersonalText => "personal text".to_string(),
        crate::CoverClass::SchemaField => "schema field".to_string(),
        crate::CoverClass::Marked => "your redaction".to_string(),
        crate::CoverClass::Other => label.replace('_', " "),
    }
}

/// A region's tag over a small picture: "face", "text", "schema".
///
/// # Example
///
/// ```rust
/// use adk_surface::CoverClass;
///
/// assert_eq!(adk_surface::words::region_short_tag(CoverClass::PersonalText, "personal_text"), "text");
/// ```
pub fn region_short_tag(class: crate::CoverClass, label: &str) -> String {
    match class {
        crate::CoverClass::Face => "face".to_string(),
        crate::CoverClass::PersonalText => "text".to_string(),
        crate::CoverClass::SchemaField => "schema".to_string(),
        crate::CoverClass::Marked => "yours".to_string(),
        crate::CoverClass::Other => label.replace('_', " "),
    }
}

/// Which of several images a class was covered in: "in image 3", "in images 1 and 3".
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::in_images(&[3]), "in image 3");
/// assert_eq!(adk_surface::words::in_images(&[1, 2, 3]), "in images 1, 2 and 3");
/// ```
pub fn in_images(which: &[usize]) -> String {
    match which {
        [one] => format!("in image {one}"),
        [rest @ .., last] => {
            let rest: Vec<String> = rest.iter().map(ToString::to_string).collect();
            format!("in images {} and {last}", rest.join(", "))
        }
        [] => String::new(),
    }
}

/// A data class as a person reads it: `document_crop` is "Document crop".
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::data_class("document_crop"), "Document crop");
/// ```
pub fn data_class(class: &str) -> String {
    let spaced = class.replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Several images of a data class: "3 document crops".
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::data_class_many("document_crop", 3), "3 document crops");
/// ```
pub fn data_class_many(class: &str, count: usize) -> String {
    format!("{count} {}s", class.replace(['_', '-'], " "))
}

/// What qualifies crops: "never the page".
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::data_class_note("document_crop").as_deref(), Some("never the page"));
/// assert_eq!(adk_surface::words::data_class_note("photo"), None);
/// ```
pub fn data_class_note(class: &str) -> Option<String> {
    class.ends_with("_crop").then(|| "never the page".to_string())
}

/// Micro-USD to the cent, or to a hundredth of a cent under a cent: "$0.0044", "$1.84".
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::money(4_400), "$0.0044");
/// assert_eq!(adk_surface::words::money(1_840_000), "$1.84");
/// ```
pub fn money(micro_usd: u64) -> String {
    if micro_usd >= 10_000 {
        return dollars(micro_usd);
    }
    let hundredths = micro_usd.saturating_add(50) / 100;
    format!("$0.{hundredths:04}")
}

/// The preflight's hold: "Up to $0.0044" (board choice A20).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::up_to(4_400), "Up to $0.0044");
/// ```
pub fn up_to(micro_usd: u64) -> String {
    format!("Up to {}", money(micro_usd))
}

/// The hold of a call with nothing left to send: "Up to $0.0012 if sent".
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::up_to_if_sent(1_200), "Up to $0.0012 if sent");
/// ```
pub fn up_to_if_sent(micro_usd: u64) -> String {
    format!("Up to {} if sent", money(micro_usd))
}

/// "Kept as evidence until 18 Dec" (board choice S5).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::kept_as_evidence_until("18 Dec"), "Kept as evidence until 18 Dec");
/// ```
pub fn kept_as_evidence_until(day: &str) -> String {
    format!("Kept as evidence until {day}")
}

/// "As evidence until 18 Dec", under "Kept" on a decided card.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::as_evidence_until("18 Dec"), "As evidence until 18 Dec");
/// ```
pub fn as_evidence_until(day: &str) -> String {
    format!("As evidence until {day}")
}

/// "This image was deleted on 18 Dec".
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::image_deleted_on("18 Dec"), "This image was deleted on 18 Dec");
/// ```
pub fn image_deleted_on(day: &str) -> String {
    format!("This image was deleted on {day}")
}

/// "Image deleted 18 Dec", over a small picture.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::image_deleted_short("18 Dec"), "Image deleted 18 Dec");
/// ```
pub fn image_deleted_short(day: &str) -> String {
    format!("Image deleted {day}")
}

/// "Image deleted on 18 Dec", a step of how a card got here.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::image_deleted_step("18 Dec"), "Image deleted on 18 Dec");
/// ```
pub fn image_deleted_step(day: &str) -> String {
    format!("Image deleted on {day}")
}

/// Why a deleted image's facts remain (board choice A13).
///
/// # Example
///
/// ```rust
/// assert!(adk_surface::words::evidence_kept_for(30).starts_with("Evidence is kept for 30 days."));
/// ```
pub fn evidence_kept_for(days: u32) -> String {
    format!(
        "Evidence is kept for {days} days. What it was, where it was going and what was covered stay on record."
    )
}

/// The notice on a card whose image is gone (board choice A13).
///
/// # Example
///
/// ```rust
/// assert!(adk_surface::words::nothing_to_send_purged("Document desk").ends_with("ask Document desk again if you still need the answer."));
/// ```
pub fn nothing_to_send_purged(agent: &str) -> String {
    format!(
        "The image this decision is about is gone, so it can't be allowed. Deny it, and ask {agent} again if you still need the answer."
    )
}

/// The notice on a card whose image is covered in full (board choice A15).
///
/// # Example
///
/// ```rust
/// assert_eq!(
///     adk_surface::words::everything_covered(Some("Gemini")),
///     "Gemini would receive a black rectangle, so there is nothing to allow. Deny it; covering can't be undone here."
/// );
/// ```
pub fn everything_covered(provider: Option<&str>) -> String {
    format!(
        "{} would receive a black rectangle, so there is nothing to allow. Deny it; covering can't be undone here.",
        provider.unwrap_or("The receiver")
    )
}

/// Allows every step of a screen plan, once each, in order (spec 3.7 Q8).
pub const ALLOW_THIS_PLAN: &str = "Allow this plan";
/// What one answer on a screen plan's card does (spec 3.7 Q8).
pub const SCREEN_PLAN_DETAIL: &str = "One answer runs every step once, in order. Each step is checked against this preview first; a step whose target moved is asked again.";
/// Why a surface that cannot show a whole screen plan sends the person to the home.
pub const LOOK_AT_THE_PLAN_IN_THE_HOME: &str =
    "This screen can't show the whole plan. Look at it in the home before you allow it.";
/// Why an open screen plan's card closed when everything was stopped (spec 3.7 Q8, V-I17).
pub const STOP_ALL_CLOSED_THE_PLAN: &str = "Stop all closed this screen plan before anyone answered it. Preview the actions again for a fresh card.";

/// The heading of a screen plan's card: "Run 4 desktop actions in app.forms, window
/// "Customer form"" (spec 3.7 Q8).
///
/// # Example
///
/// ```rust
/// assert_eq!(
///     adk_surface::words::screen_plan_heading(4, "app.forms, window \"Customer form\""),
///     "Run 4 desktop actions in app.forms, window \"Customer form\""
/// );
/// ```
pub fn screen_plan_heading(steps: usize, window: &str) -> String {
    format!("Run {steps} desktop actions in {window}")
}

/// The heading of a screen plan's card in chat, which names no window (S-I5): "Run 4 desktop
/// actions".
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::screen_plan_chat_heading(4), "Run 4 desktop actions");
/// ```
pub fn screen_plan_chat_heading(steps: usize) -> String {
    format!("Run {steps} desktop actions")
}

/// The line of a decision a person changed before it ran, its call held again: "You changed
/// it at 10:15 · held again with the new image" (spec 3.7 V-I5).
///
/// # Example
///
/// ```rust
/// assert_eq!(
///     adk_surface::words::held_again_at("10:15"),
///     "You changed it at 10:15 · held again with the new image"
/// );
/// ```
pub fn held_again_at(hh_mm: &str) -> String {
    format!("You changed it at {hh_mm} · {HELD_AGAIN_WITH_THE_NEW_IMAGE}")
}

/// The run that waited on a card ended: "Waited 10 minutes; the run ended at 10:24"
/// (board choice A12).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_surface::words::run_ended(10, "10:24"), "Waited 10 minutes; the run ended at 10:24");
/// assert_eq!(adk_surface::words::run_ended(1, "10:15"), "Waited 1 minute; the run ended at 10:15");
/// ```
pub fn run_ended(minutes: i64, hh_mm: &str) -> String {
    let unit = if minutes == 1 { "minute" } else { "minutes" };
    format!("Waited {minutes} {unit}; the run ended at {hh_mm}")
}

/// The notice on a card whose run ended while it waited (board choice A12).
///
/// # Example
///
/// ```rust
/// assert!(adk_surface::words::stopped_waiting("Document desk", "10:24").starts_with("Document desk stopped waiting at 10:24"));
/// ```
pub fn stopped_waiting(agent: &str, hh_mm: &str) -> String {
    format!(
        "{agent} stopped waiting at {hh_mm} and finished its run without this. Your answer is kept and used when {agent} picks this thread up again."
    )
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
        ANSWER,
        LOOK_ALIKE_ADDRESS,
        HIDDEN_HOST_ADDRESS,
        UNENCRYPTED_ADDRESS,
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
        A_DECISION_IS_WAITING,
        OPEN_THE_APP_TO_SEE_IT,
        YOUR_PHONE,
        USE_YOUR_FACE_OR_FINGERPRINT,
        USE_YOUR_FACE_OR_FINGERPRINT_ON_THIS_COMPUTER,
        ALLOW_NOTIFICATIONS,
        ADD_TO_HOME_SCREEN_FIRST,
        APPROVE_IT_IN_THE_HOME,
        LISTENING,
        SPEAKING,
        SAY_ALLOW_ONCE_OR_DENY,
        SAY_DENY_OR_DECIDE_ON_SCREEN,
        DECIDE_ON_SCREEN,
        CANCEL,
        END,
        WHAT_IT_CAN_SEE,
        VOICE_PRIVACY_NOTE,
        SPOKEN_ANSWER_CANCELLED,
        CHANGED_WHILE_YOU_SPOKE,
        VOICE_CANT_APPROVE_THIS,
        STOPPING_NEEDS_THE_OPERATOR,
        APP_CALL_SENT,
        APP_SERVER_DIDNT_ANSWER,
        THE_EXACT_IMAGE_THAT_WOULD_LEAVE,
        THE_IMAGE_THAT_LEFT,
        THE_IMAGE_THAT_WOULD_HAVE_LEFT,
        NOTHING_TO_SEND,
        EVERYTHING_IS_COVERED,
        IMAGE_DELETED,
        IMAGE_DELETED_EARLY,
        IMAGES_LEAVE_ONLY_COVERED,
        LOOK_AT_THE_IMAGE_ON_A_SCREEN,
        SHOW_THE_IMAGE_IN_THE_HOME,
        OPEN_THE_SOURCE_IMAGE,
        ALLOW_THIS_PLAN,
        SCREEN_PLAN_DETAIL,
        LOOK_AT_THE_PLAN_IN_THE_HOME,
        STOP_ALL_CLOSED_THE_PLAN,
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
