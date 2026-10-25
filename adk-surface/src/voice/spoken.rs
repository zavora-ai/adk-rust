//! What a person's words mean to the voice view (spec 3.3 §5.6, §7.2, §7.4 rule 4).
//!
//! A spoken answer is one more way to give an answer the engine already has, under the same
//! rules as a click or a key, so voice has no words of its own: the person says the label of
//! the engine's action. Anything else — a conversational "sure", a "yes", an echo of the
//! prompt, a sentence that mentions two answers — decides nothing.
//!
//! | Said | Means | When |
//! |---|---|---|
//! | "allow once" | [`Spoken::AllowOnce`]: the engine's **Allow once**, after its cancel window | a decision is prompted |
//! | "deny", "deny it" | [`Spoken::Deny`]: the engine's **Deny** | a decision is prompted |
//! | "cancel" | [`Spoken::Cancel`]: **Cancel** on the pending spoken answer | a spoken answer is pending |
//! | "stop all" | [`Spoken::StopAll`]: **Stop all** | always; matched first |
//!
//! "please" may come anywhere in any of them. The transcript is lower-cased and split on
//! everything that is not a letter, a digit or an apostrophe, so case and punctuation never
//! change what it means. Then, after "stop all" is matched:
//!
//! - an utterance of more than [`MAX_WORDS`] words decides nothing;
//! - an utterance that contains "always" decides nothing (§7.4 rule 4: voice never says
//!   "always");
//! - what is left after "please" must be exactly one phrase of the table: "allow once or
//!   deny", "yes allow once", "allow once deny" and "allow" decide nothing.
//!
//! "stop" and "cancel" are never a denial, and "yes", "ok", "sure" and "go ahead" are never an
//! approval: the harness's own `parse_spoken_decision` accepts them and is not used here.

/// The most words an utterance that decides something may have.
pub const MAX_WORDS: usize = 4;

/// What an utterance asks for (spec 3.3 §5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Spoken {
    /// "allow once": allow the prompted call once, after the cancel window.
    AllowOnce,
    /// "deny": refuse the prompted call.
    Deny,
    /// "cancel": cancel the spoken answer in its cancel window.
    Cancel,
    /// "stop all": stop every agent.
    StopAll,
}

/// The filler every phrase may carry.
const PLEASE: &str = "please";

/// Reads `transcript`, a completed input transcript of the person's speech, as one of the
/// engine's answers; `pending` says whether a spoken answer waits in its cancel window, the
/// only time "cancel" means anything (spec 3.3 §5.6).
///
/// Returns `None` for anything that is not plainly one of them: nothing is decided from an
/// ambiguous utterance.
///
/// # Example
///
/// ```rust
/// use adk_surface::voice::spoken::{Spoken, parse};
///
/// assert_eq!(parse("Allow once.", false), Some(Spoken::AllowOnce));
/// assert_eq!(parse("deny it", false), Some(Spoken::Deny));
/// assert_eq!(parse("Stop all!", false), Some(Spoken::StopAll));
/// assert_eq!(parse("cancel", true), Some(Spoken::Cancel));
/// assert_eq!(parse("cancel", false), None);
/// assert_eq!(parse("yes", false), None);
/// assert_eq!(parse("always allow", false), None);
/// assert_eq!(parse("say allow once or deny", false), None);
/// ```
pub fn parse(transcript: &str, pending: bool) -> Option<Spoken> {
    let words: Vec<String> = transcript
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let phrase: Vec<&str> =
        words.iter().map(String::as_str).filter(|word| *word != PLEASE).collect();
    if phrase == ["stop", "all"] {
        return Some(Spoken::StopAll);
    }
    if words.is_empty() || words.len() > MAX_WORDS || words.iter().any(|word| word == "always") {
        return None;
    }
    match phrase.as_slice() {
        ["allow", "once"] => Some(Spoken::AllowOnce),
        ["deny"] | ["deny", "it"] => Some(Spoken::Deny),
        ["cancel"] if pending => Some(Spoken::Cancel),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table of the brief and the owner's answer: only the engine's labels decide.
    #[test]
    fn only_the_engines_answers_decide() {
        let cases: &[(&str, bool, Option<Spoken>)] = &[
            ("Allow once.", false, Some(Spoken::AllowOnce)),
            ("allow once please", false, Some(Spoken::AllowOnce)),
            ("Please, allow once!", false, Some(Spoken::AllowOnce)),
            ("deny", false, Some(Spoken::Deny)),
            ("Deny it.", false, Some(Spoken::Deny)),
            ("deny, please", true, Some(Spoken::Deny)),
            ("always allow", false, None),
            ("allow once always", false, None),
            ("say allow once or deny", false, None),
            ("allow once or deny", false, None),
            ("allow once deny", false, None),
            ("sure", false, None),
            ("yes", false, None),
            ("yes, allow once", false, None),
            ("ok", false, None),
            ("go ahead", false, None),
            ("no", false, None),
            ("stop", false, None),
            ("stop", true, None),
            ("allow", false, None),
            ("once", false, None),
            ("cancel", false, None),
            ("Cancel.", true, Some(Spoken::Cancel)),
            ("cancel it", true, None),
            ("wait", true, None),
            ("stop that", true, None),
            ("stop all", false, Some(Spoken::StopAll)),
            ("Stop all, please.", true, Some(Spoken::StopAll)),
            ("please please allow once please", false, None),
            ("", false, None),
            ("...", false, None),
        ];
        for (said, pending, expected) in cases {
            assert_eq!(parse(said, *pending), *expected, "{said:?} (pending: {pending})");
        }
    }

    #[test]
    fn stop_all_is_never_a_denial() {
        for said in ["stop all", "STOP ALL", "stop all please"] {
            assert_eq!(parse(said, false), Some(Spoken::StopAll));
            assert_eq!(parse(said, true), Some(Spoken::StopAll));
        }
    }
}
