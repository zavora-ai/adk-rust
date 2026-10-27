//! The words of §8 for the web home, generated from [`words`](crate::words) so the page and
//! the engine say the same thing (spec 3.3 §8). `scripts/check-surface-api.sh` writes them to
//! `adk-surface/web/src/generated/words.ts` and CI fails when they drift.
//!
//! **Every word in every build.** Each constant of [`words`](crate::words) is unconditional,
//! whichever surface says it, so `words.ts` is the same in every build (see
//! [`openapi`](fn@super::openapi) for the same rule on wire types).

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
    IN_THE_HOME_APP,
    ON_YOUR_PHONE,
    IN_THE_TERMINAL,
    FROM_THE_TRAY,
    BY_VOICE,
    ALL_AGENTS_STOPPED,
    ALL_AGENTS_STOPPED_RESUME_IN_HOME,
    SENT_AFTER_THIS_CARD_IS_ANSWERED,
    SENT_WHEN_YOUR_AGENT_IS_FREE,
    YOU_DECIDED,
    RIGHT_AWAY,
    YOU_SUGGESTED_THIS,
    A_DECISION_IS_WAITING,
    OPEN_THE_APP_TO_SEE_IT,
    YOUR_PHONE,
    ASKS_FOR_FACE_OR_FINGERPRINT,
    NEEDS_FACE_OR_FINGERPRINT,
    DONE_ONCE,
    WANTS_TO_RUN_THIS_COMMAND,
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
    NEW_AGENT,
    DESCRIBE_IT,
    SET_IT_UP_YOURSELF,
    ASK_TO_SUGGEST_ONE,
    ADD_THIS_AGENT,
    CONFIRM_ITS_YOU_AND_ADD,
    CHANGE_IT_FIRST,
    DONT_ADD,
    SAVE_CHANGES,
    COPY_ONE_OF_YOUR_RULES,
    PAUSE,
    RESUME,
    REMOVE,
    KEEP,
    READY,
    PAUSED,
    CANT_START,
    YOU_CAN_REMOVE_IT_LATER,
    YOU_CAN_CHANGE_IT_BACK,
    ON_THIS_HOME_IT_CAN_ALSO,
    NOTHING_YET,
    NO_AGENTS_OF_YOUR_OWN,
    WHAT_SHOULD_IT_DO,
    SUGGEST_A_NEW_AGENT_FOR,
    REMOVED_AGENTS,
    NOT_SENT_YET,
    SENDING,
    SENT_YOU_SAID_NO,
    SENT_FAILED,
    FORM_CHANGED_SEND_AGAIN,
    SENDING_ASKS_YOU_FIRST,
    SHOW_FORMS_AND_TABLES,
    SEND_FORMS_FROM_A_SCREEN,
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
