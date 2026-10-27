//! A screen action said aloud (spec 3.3 §5.6, owner question 26): "snooze news digest".
//!
//! The voice view reads a completed transcript for a screen action only when it is not one of
//! the engine's answers (`voice::spoken::parse` found nothing)
//! and not the model's own audio played back. Then:
//!
//! - an utterance of more than [`MAX_WORDS`] words, or with "always" in it, does nothing;
//! - "please" may come anywhere;
//! - it starts with the action's phrase, "snooze", and names exactly one title in the
//!   person's Coming up, ignoring case and punctuation; "tomorrow" at the end moves it to the
//!   same time tomorrow, else an hour later;
//! - words that are part of several titles are ambiguous, and nothing runs.
//!
//! A matched action waits out the engine's cancel window, where "cancel" withdraws it.
//!
//! # Example
//!
//! ```rust
//! use adk_surface::verbs::ComingUpItem;
//! use adk_surface::verbs::spoken::{SpokenAction, parse};
//!
//! let item = |id: &str, title: &str| ComingUpItem {
//!     id: id.into(), title: title.into(), area: "work".into(), at: None,
//!     at_text: String::new(), ends: None, choices: vec![], why_not: None,
//! };
//! let items = [item("a", "News digest"), item("b", "Weekly digest"), item("c", "Water plants")];
//! assert_eq!(
//!     parse("Snooze news digest, please.", &items),
//!     Some(SpokenAction::Snooze { item: "a".into(), title: "News digest".into(), tomorrow: false })
//! );
//! assert!(matches!(parse("snooze digest", &items), Some(SpokenAction::Ambiguous { .. })));
//! assert_eq!(parse("always snooze news digest", &items), None);
//! assert_eq!(parse("snooze", &items), None);
//! ```

use super::ComingUpItem;
use super::snooze::normalize;

/// The most words a spoken screen action may have.
pub const MAX_WORDS: usize = 8;

/// The phrase that starts a spoken snooze.
const SNOOZE: &str = "snooze";

/// What a spoken screen action asks for (spec 3.3 §5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpokenAction {
    /// Snooze the item `item`, titled `title`: to the same time tomorrow, or an hour later.
    Snooze {
        /// The item's id.
        item: String,
        /// Its title.
        title: String,
        /// To the same time tomorrow.
        tomorrow: bool,
    },
    /// The words match several titles: "Two things match “digest” — say the full name or use
    /// the screen".
    Ambiguous {
        /// The words said after the action's phrase.
        said: String,
        /// The titles that match.
        matches: Vec<String>,
    },
}

/// Reads `transcript` as a spoken screen action on one of `items`, the person's Coming up.
///
/// Returns `None` for anything that is not plainly one: nothing runs from an utterance that
/// says more, says "always", or names no title.
pub fn parse(transcript: &str, items: &[ComingUpItem]) -> Option<SpokenAction> {
    let words: Vec<String> = normalize(transcript)
        .split(' ')
        .filter(|word| !word.is_empty())
        .map(str::to_string)
        .collect();
    if words.is_empty() || words.len() > MAX_WORDS || words.iter().any(|word| word == "always") {
        return None;
    }
    let mut phrase: Vec<&str> =
        words.iter().map(String::as_str).filter(|word| *word != "please").collect();
    if phrase.first() != Some(&SNOOZE) {
        return None;
    }
    phrase.remove(0);
    let tomorrow = phrase.last() == Some(&"tomorrow");
    if tomorrow {
        phrase.pop();
        if phrase.last() == Some(&"until") {
            phrase.pop();
        }
    }
    let said = phrase.join(" ");
    if said.is_empty() {
        return None;
    }
    let exact: Vec<&ComingUpItem> =
        items.iter().filter(|item| normalize(&item.title) == said).collect();
    match exact.as_slice() {
        [only] => {
            return Some(SpokenAction::Snooze {
                item: only.id.clone(),
                title: only.title.clone(),
                tomorrow,
            });
        }
        [] => {}
        several => {
            return Some(SpokenAction::Ambiguous {
                said,
                matches: several.iter().map(|item| item.title.clone()).collect(),
            });
        }
    }
    let partial: Vec<&ComingUpItem> =
        items.iter().filter(|item| normalize(&item.title).contains(&said)).collect();
    (partial.len() > 1).then(|| SpokenAction::Ambiguous {
        said,
        matches: partial.iter().map(|item| item.title.clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, title: &str) -> ComingUpItem {
        ComingUpItem {
            id: id.into(),
            title: title.into(),
            area: "work".into(),
            at: None,
            at_text: String::new(),
            ends: None,
            choices: vec![],
            why_not: None,
        }
    }

    #[test]
    fn only_a_plain_snooze_of_one_title_runs() {
        let items =
            [item("a", "News digest"), item("b", "Weekly digest"), item("c", "Water plants")];
        let snooze = |id: &str, title: &str, tomorrow| {
            Some(SpokenAction::Snooze { item: id.into(), title: title.into(), tomorrow })
        };
        let cases: &[(&str, Option<SpokenAction>)] = &[
            ("Snooze news digest", snooze("a", "News digest", false)),
            ("please snooze News Digest.", snooze("a", "News digest", false)),
            ("snooze water plants tomorrow", snooze("c", "Water plants", true)),
            ("snooze water plants until tomorrow please", snooze("c", "Water plants", true)),
            (
                "snooze digest",
                Some(SpokenAction::Ambiguous {
                    said: "digest".into(),
                    matches: vec!["News digest".into(), "Weekly digest".into()],
                }),
            ),
            ("snooze", None),
            ("snooze laundry", None),
            ("snooze news", None),
            ("always snooze news digest", None),
            ("snooze news digest always", None),
            ("could you please snooze the news digest for me", None),
            ("snooze news digest and water plants", None),
            ("news digest", None),
            ("allow once", None),
            ("", None),
        ];
        for (said, expected) in cases {
            assert_eq!(&parse(said, &items), expected, "{said:?}");
        }
    }
}
