//! `/snooze <title> [tomorrow]` in a terminal (spec 3.3 §5.2, owner question 27): typed by the
//! person, run as the terminal's caller.
//!
//! The terminal in process runs it through [`SnoozeCommand`]; a terminal attached to
//! `adk-rust up` reaches the same code through the kernel's command host, in up's process. A
//! terminal paired from another machine cannot snooze: the kernel's commands need its own
//! socket or the operator's token, so it says to use the home. Text an agent writes, a
//! "/snooze" in a reply included, is never a command.

use super::{ObjectRef, VerbError, VerbGate, VerbIntent, find_by_title, words};
use crate::origin::Caller;

/// What `/snooze` was given: a title, and whether to the same time tomorrow.
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::{SnoozeText, parse_snooze};
///
/// assert_eq!(parse_snooze("News digest"), Some(SnoozeText { title: "News digest".into(), tomorrow: false }));
/// assert_eq!(parse_snooze("Water plants Tomorrow"), Some(SnoozeText { title: "Water plants".into(), tomorrow: true }));
/// assert_eq!(parse_snooze("  "), None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnoozeText {
    /// The title, as typed.
    pub title: String,
    /// To the same time tomorrow, else an hour later.
    pub tomorrow: bool,
}

/// Reads the text after `/snooze`: a title, and `tomorrow` as its last word for the same time
/// tomorrow. `None` without a title.
pub fn parse_snooze(args: &str) -> Option<SnoozeText> {
    let args = args.trim();
    let (title, tomorrow) = match args.rsplit_once(char::is_whitespace) {
        Some((title, last)) if last.eq_ignore_ascii_case("tomorrow") => (title.trim_end(), true),
        _ if args.eq_ignore_ascii_case("tomorrow") => ("", true),
        _ => (args, false),
    };
    (!title.is_empty()).then(|| SnoozeText { title: title.to_string(), tomorrow })
}

/// Runs `/snooze <args>` for `caller`: "✓ snoozed News digest → 19:00" once it ran.
///
/// # Errors
///
/// [`VerbError::Invalid`] without a title; [`VerbError::NotFound`] when no title matches;
/// [`VerbError::Ambiguous`] when several do; [`VerbError::Refused`] naming the item when it
/// cannot be snoozed: "can't snooze CI check: it has already started"; the gate's other
/// errors. [`problem_line`] words each for the terminal.
///
/// # Example
///
/// ```rust,no_run
/// use adk_surface::{Caller, Origin, SurfaceKind};
///
/// # async fn example(gate: adk_surface::verbs::VerbGate) {
/// let ana = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard).with_paired_local();
/// match adk_surface::verbs::snooze_text(&gate, &ana, "News digest").await {
///     Ok(line) => println!("{line}"),
///     Err(error) => println!("{}", adk_surface::verbs::problem_line(&error)),
/// }
/// # }
/// ```
pub async fn snooze_text(
    gate: &VerbGate,
    caller: &Caller,
    args: &str,
) -> Result<String, VerbError> {
    let Some(typed) = parse_snooze(args) else {
        return Err(VerbError::invalid(
            "/snooze takes a title from Coming up: /snooze <title> [tomorrow]",
        ));
    };
    let items = gate.coming_up(caller).await?;
    let item = find_by_title(&items, &typed.title)?;
    let refused = |why: &str| VerbError::refused(format!("can't snooze {}: {why}", item.title));
    if let Some(why) = &item.why_not {
        return Err(refused(why));
    }
    let Some(choice) = item.choices.iter().find(|choice| choice.tomorrow == typed.tomorrow) else {
        let why = item.ends.map_or_else(
            || words::NOT_ONE_TIME.to_string(),
            |ends| words::ends_at(&gate.registry().scope(caller.user_id(), None).time_text(ends)),
        );
        return Err(refused(&why));
    };
    let intent = VerbIntent::new("snooze", serde_json::json!({ "to": choice.to }))
        .on(ObjectRef::coming_up(&item.id));
    match gate.call(caller, intent).await {
        Ok(_) => Ok(format!("✓ {}", words::snoozed(&item.title, &choice.at_text))),
        Err(VerbError::Refused(why)) => Err(refused(&why)),
        Err(error) => Err(error),
    }
}

/// `error` as a terminal says it: "nothing in Coming up is called “laundry”", "two things
/// match “digest” — type the full title:" and the titles, one per line.
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::{VerbError, problem_line};
///
/// let error = VerbError::Ambiguous { said: "digest".into(), matches: vec!["News digest".into(), "Weekly digest".into()] };
/// assert_eq!(problem_line(&error), "two things match “digest” — type the full title:\n  News digest\n  Weekly digest");
/// ```
pub fn problem_line(error: &VerbError) -> String {
    match error {
        VerbError::Ambiguous { said, matches } => {
            let head = words::things_match(matches.len(), said);
            let mut chars = head.chars();
            let head: String = match chars.next() {
                Some(first) => first.to_lowercase().chain(chars).collect(),
                None => head,
            };
            let list: Vec<String> = matches.iter().map(|title| format!("  {title}")).collect();
            format!("{head} — type the full title:\n{}", list.join("\n"))
        }
        other => other.to_string(),
    }
}

/// `/snooze` in the terminal (spec 3.3 §5.2): in process, through a [`VerbGate`] as the
/// terminal's caller; or, on a terminal paired from another machine, only the words that say
/// to use the home.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use adk_surface::verbs::SnoozeCommand;
/// use adk_surface::{Caller, Origin, SurfaceKind};
///
/// # fn example(gate: adk_surface::verbs::VerbGate, terminal: adk_surface::tui::TerminalSurface) {
/// let ana = Caller::human("ana", SurfaceKind::Terminal, Origin::Keyboard).with_paired_local();
/// let terminal = terminal.command(Arc::new(SnoozeCommand::new(gate, ana)));
/// # let _ = terminal;
/// # }
/// ```
#[cfg(feature = "tui")]
pub struct SnoozeCommand {
    reach: Option<(VerbGate, Caller)>,
}

#[cfg(feature = "tui")]
impl SnoozeCommand {
    /// `/snooze` through `gate`, as `caller`.
    pub fn new(gate: VerbGate, caller: Caller) -> Self {
        Self { reach: Some((gate, caller)) }
    }

    /// `/snooze` on a terminal paired from another machine: "Snooze isn't available from a
    /// remote terminal — use the home."
    pub fn remote() -> Self {
        Self { reach: None }
    }
}

#[cfg(feature = "tui")]
#[async_trait::async_trait]
impl crate::tui::TerminalCommand for SnoozeCommand {
    fn name(&self) -> &str {
        "snooze"
    }

    fn description(&self) -> &str {
        words::SNOOZE_SOMETHING
    }

    async fn run(
        &self,
        args: &str,
        _context: &crate::tui::CommandContext,
    ) -> adk_core::Result<String> {
        let Some((gate, caller)) = &self.reach else {
            return Err(adk_core::AdkError::from(VerbError::NotHere(
                words::NOT_FROM_A_REMOTE_TERMINAL.to_string(),
            )));
        };
        snooze_text(gate, caller, args).await.map_err(|error| {
            let line = problem_line(&error);
            let mut error = adk_core::AdkError::from(error);
            error.message = line;
            error
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_and_tomorrow_are_read_from_what_was_typed() {
        let cases: &[(&str, Option<(&str, bool)>)] = &[
            ("News digest", Some(("News digest", false))),
            ("  News digest  ", Some(("News digest", false))),
            ("news digest tomorrow", Some(("news digest", true))),
            ("News digest TOMORROW", Some(("News digest", true))),
            ("Tomorrow's plan", Some(("Tomorrow's plan", false))),
            ("tomorrow", None),
            ("", None),
        ];
        for (typed, expected) in cases {
            let expected =
                expected.map(|(title, tomorrow)| SnoozeText { title: title.into(), tomorrow });
            assert_eq!(parse_snooze(typed), expected, "{typed:?}");
        }
    }
}
