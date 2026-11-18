//! Dark patterns in a drawing (spec 3.3 §7.4 rule 5, extended at warning severity): wording
//! that pressures a person instead of informing them. Models add it unprompted — false
//! urgency, scarcity, countdowns, fake social proof, confirmshaming, and text that passes for
//! the home's own controls. A drawing that has it is still shown; the drawing agent is told
//! what to change as A2UI 1.0 `ValidationResult` warnings.
//!
//! The lists are a first, measured step: every phrase is matched on folded words (case,
//! look-alike and zero-width characters folded as rule 5 folds them), and the sample drawings
//! raise none. A phrase earns a refusal only once its false positives are known.

use std::sync::LazyLock;

use adk_harness::approvals::matcher::fold_display_text;

use super::validate::{ResolvedText, ValidationResult};

/// One kind of pressure, its code, what it says, and its phrases.
struct Kind {
    code: &'static str,
    what: &'static str,
    phrases: &'static [&'static str],
}

const KINDS: &[Kind] = &[
    Kind {
        code: "genui.warned.urgency",
        what: "presses for haste",
        phrases: &[
            "act now",
            "hurry",
            "limited time",
            "last chance",
            "don't miss out",
            "dont miss out",
            "ends tonight",
            "ends today",
            "final hours",
            "only today",
            "before it's too late",
            "before its too late",
            "offer expires",
            "expires soon",
            "right now or",
        ],
    },
    Kind {
        code: "genui.warned.scarcity",
        what: "claims scarcity",
        phrases: &[
            "left in stock",
            "almost gone",
            "selling fast",
            "while supplies last",
            "few remaining",
            "only a few left",
            "going fast",
            "low stock",
        ],
    },
    Kind {
        code: "genui.warned.social_proof",
        what: "claims others are acting",
        phrases: &[
            "people are viewing",
            "people are looking",
            "people bought",
            "others bought",
            "people just bought",
            "everyone is buying",
            "in their carts",
            "in their cart",
            "trending now",
        ],
    },
    Kind {
        code: "genui.warned.confirmshaming",
        what: "shames saying no",
        phrases: &[
            "no thanks i don't",
            "no thanks i dont",
            "no i don't want",
            "no i dont want",
            "i prefer to pay more",
            "i'd rather miss",
            "i like losing",
        ],
    },
    Kind {
        code: "genui.warned.host_imitation",
        what: "passes for the home's own controls",
        phrases: &[
            "system message",
            "security check",
            "session expired",
            "your session has expired",
            "sign in to continue",
            "verify your identity",
            "verified by adk",
            "official notice",
            "home security",
        ],
    },
];

/// Each kind's phrases as folded words, folded once.
/// A kind and its phrases as folded words.
type FoldedKind = (&'static Kind, Vec<Vec<String>>);

static FOLDED: LazyLock<Vec<FoldedKind>> = LazyLock::new(|| {
    KINDS
        .iter()
        .map(|kind| {
            let phrases =
                kind.phrases.iter().map(|phrase| words(&fold_display_text(phrase))).collect();
            (kind, phrases)
        })
        .collect()
});

fn words(folded: &str) -> Vec<String> {
    folded
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|word| word.trim_matches('\'').to_string())
        .filter(|word| !word.is_empty())
        .collect()
}

/// Whether `text` counts down: "ends in 04:59", "expires in 10 minutes", "00:59 left".
fn counts_down(words: &[String], text: &str) -> bool {
    let clock = text.split_whitespace().any(|token| {
        let token = token.trim_matches(|c: char| !c.is_ascii_digit() && c != ':');
        let parts: Vec<&str> = token.split(':').collect();
        parts.len() >= 2
            && parts.iter().all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_digit()))
    });
    let ends_in = words
        .windows(2)
        .any(|pair| matches!(pair[0].as_str(), "ends" | "expires" | "closes") && pair[1] == "in");
    let left = words.iter().any(|word| word == "left" || word == "remaining");
    (clock && (ends_in || left))
        || (ends_in
            && words.iter().any(|word| word.starts_with("minute") || word.starts_with("second")))
}

/// The warnings `texts` raise, at most one per kind, each naming where it was found.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{ResolvedText, Severity, dark_patterns};
///
/// let text = |field: &str, text: &str| ResolvedText {
///     component_id: Some("offer".into()),
///     item: None,
///     field: field.into(),
///     text: text.into(),
/// };
/// let warnings = dark_patterns(&[text("text", "Only 2 left in stock — hurry!")]);
/// assert_eq!(warnings.len(), 2);
/// assert!(warnings.iter().all(|w| w.valid && w.severity == Severity::Warning));
/// assert!(dark_patterns(&[text("text", "Q3 revenue rose 12%")]).is_empty());
/// ```
pub fn dark_patterns(texts: &[ResolvedText]) -> Vec<ValidationResult> {
    let mut warnings = Vec::new();
    let mut seen = Vec::new();
    for resolved in texts {
        let folded = fold_display_text(&resolved.text);
        let found = words(&folded);
        let at = || match &resolved.component_id {
            Some(id) => format!("component {id} · {}", resolved.field),
            None => resolved.field.clone(),
        };
        for (kind, phrases) in FOLDED.iter() {
            if seen.contains(&kind.code) {
                continue;
            }
            let hit = phrases.iter().find(|phrase| {
                !phrase.is_empty()
                    && found.windows(phrase.len()).any(|window| window == phrase.as_slice())
            });
            if let Some(phrase) = hit {
                seen.push(kind.code);
                warnings.push(ValidationResult::warning(
                    kind.code,
                    format!(
                        "{}: \"{}\" {}. Say what is true plainly, or leave it out.",
                        at(),
                        phrase.join(" "),
                        kind.what
                    ),
                ));
            }
        }
        if !seen.contains(&"genui.warned.countdown") && counts_down(&found, &resolved.text) {
            seen.push("genui.warned.countdown");
            warnings.push(ValidationResult::warning(
                "genui.warned.countdown",
                format!(
                    "{}: a countdown presses for haste. Give the deadline as a date instead.",
                    at()
                ),
            ));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> ResolvedText {
        ResolvedText {
            component_id: Some("c".into()),
            item: None,
            field: "text".into(),
            text: value.into(),
        }
    }

    #[test]
    fn pressure_is_found_and_plain_facts_are_not() {
        for (said, code) in [
            ("Act now to keep your price", "genui.warned.urgency"),
            ("Only 3 left in stock", "genui.warned.scarcity"),
            ("12 people are viewing this hotel", "genui.warned.social_proof"),
            ("No thanks, I don't want to save money", "genui.warned.confirmshaming"),
            ("System message: your plan needs attention", "genui.warned.host_imitation"),
            ("Offer ends in 04:59", "genui.warned.countdown"),
            ("Expires in 10 minutes", "genui.warned.countdown"),
            ("ＨＵＲＲＹ, prices rise", "genui.warned.urgency"),
        ] {
            let warnings = dark_patterns(&[text(said)]);
            assert!(
                warnings.iter().any(|w| w.code.as_deref() == Some(code)),
                "{said}: {warnings:?}"
            );
        }
        for plain in [
            "Revenue rose 12% in Q3",
            "Stock level: 40 units",
            "Deadline: 2026-11-30",
            "Meeting at 10:30",
            "3 people approved this budget last week",
            "Remaining budget: $4,200",
            "Session summary",
        ] {
            assert!(dark_patterns(&[text(plain)]).is_empty(), "{plain}");
        }
    }

    #[test]
    fn each_kind_warns_once() {
        let warnings = dark_patterns(&[text("Hurry!"), text("Act now"), text("Last chance")]);
        assert_eq!(warnings.len(), 1);
    }
}
