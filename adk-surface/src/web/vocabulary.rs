//! The words of §8 for the web home, generated from [`words`](crate::words) so the page and
//! the engine say the same thing (spec 3.3 §8). `scripts/check-surface-api.sh` writes them to
//! `adk-surface/web/src/generated/words.ts` and CI fails when they drift.

use std::fmt::Write as _;

macro_rules! listed {
    ($($name:ident),* $(,)?) => {
        &[$((stringify!($name), crate::words::$name)),*]
    };
}

/// Every fixed word of [`words`](crate::words), by its constant's name.
const WORDS: &[(&str, &str)] = listed![
    WORKING,
    WAITING_FOR_YOU,
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
    ENDED_WITHOUT_AN_ANSWER,
    IN_THE_HOME_APP,
    ON_YOUR_PHONE,
    IN_THE_TERMINAL,
    FROM_THE_TRAY,
    BY_VOICE,
    ALL_AGENTS_STOPPED,
    ALL_AGENTS_STOPPED_RESUME_IN_HOME,
    SENT_AFTER_THIS_CARD_IS_ANSWERED,
    YOU_DECIDED,
];

/// The words of [`words`](crate::words) as a TypeScript module, one exported constant each
/// (spec 3.3 §8).
///
/// # Example
///
/// ```rust
/// let module = adk_surface::web::words_typescript();
/// assert!(module.contains("export const STOP_ALL = \"Stop all\";"));
/// ```
pub fn words_typescript() -> String {
    let mut out = String::from(
        "// Generated from adk-surface's `words` module by `scripts/check-surface-api.sh --write`.\n\
         // Do not edit: the web home says what every other surface says (spec 3.3 §8).\n\n",
    );
    for (name, value) in WORDS {
        let literal = serde_json::to_string(value).unwrap_or_default();
        let _ = writeln!(out, "export const {name} = {literal};");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A word added to `words` without being listed here would never reach the page.
    #[test]
    fn every_word_is_listed() {
        let source = include_str!("../words.rs");
        let declared = source.lines().filter(|line| line.starts_with("pub const ")).count();
        assert_eq!(WORDS.len(), declared);
    }
}
