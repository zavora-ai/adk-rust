//! The words of screen actions (spec 3.3 §8): what the home, the terminal, voice and a card
//! say about them. No person-facing word says "verb" or "OS".
//!
//! # Example
//!
//! ```rust
//! use adk_surface::verbs::words;
//!
//! assert_eq!(words::SNOOZE_SOMETHING, "Snooze something in Coming up");
//! assert_eq!(words::move_to("News digest", "19:00"), "Move News digest to 19:00");
//! ```

/// The button on a Coming up row.
pub const SNOOZE: &str = "Snooze";
/// What snoozing does, and the chat heading of an agent's card.
pub const SNOOZE_SOMETHING: &str = "Snooze something in Coming up";
/// The first choice of the Snooze menu.
pub const ONE_HOUR_LATER: &str = "1 hour later";
/// The second choice of the Snooze menu.
pub const TOMORROW_SAME_TIME: &str = "Tomorrow, same time";
/// While a snooze is on its way.
pub const SNOOZING: &str = "Snoozing…";
/// The heading of the list of screen actions in Memory & rules and `/help`.
pub const ON_YOUR_SCREENS: &str = "On your screens";
/// The rule under "On your screens".
pub const AGENTS_ALWAYS_ASK_YOU_FIRST: &str = "Agents always ask you first";
/// The fixed callout under "On your screens".
pub const ASKS_YOU_FIRST_ALWAYS: &str = "When an agent wants to change something on your \
                                         screens, it asks you first. This can't be switched off.";
/// The fixed line on an agent's card.
pub const AGENTS_ALWAYS_ASK: &str = "Agents always ask before they change things on your screens.";
/// What a host grants every agent it runs, under "On this home it can also:" (spec 3.3 §5.7).
pub const GRANTED: &str = "Ask to change things on your screens — always asks you first";
/// Refused: the item's wake moved since the request was shown.
pub const IT_MOVED: &str = "It moved since you saw this";
/// Refused: the item's process started.
pub const ALREADY_STARTED: &str = "it has already started";
/// Refused: the item waits on something other than one time.
pub const NOT_ONE_TIME: &str = "it doesn't wait for one time, so it can't be snoozed";
/// Refused: the item is not scheduled yet.
pub const NOT_SCHEDULED: &str = "it isn't scheduled yet";
/// Refused: the item has ended.
pub const ENDED: &str = "it has ended";
/// Refused from a chat channel or the tray (spec 3.3 §7.2).
pub const NOT_FROM_HERE: &str = "Snoozing isn't available here — use the home";
/// Refused from a paired remote terminal (owner question 27).
pub const NOT_FROM_A_REMOTE_TERMINAL: &str =
    "Snooze isn't available from a remote terminal — use the home.";
/// Refused for an agent: it asks through its own tool.
pub const AGENTS_ASK_THROUGH_THEIR_TOOLS: &str =
    "An agent asks for a screen action through its own tool, and a person answers";
/// The voice side note (spec 3.3 §5.6).
pub const VOICE_NOTE: &str = "By voice you can snooze what's coming up, say yes once, or no, to \
                              everyday things.";
/// A spoken action cancelled in its window.
pub const CANCELLED_NOTHING_CHANGED: &str = "Cancelled · nothing changed";

/// "Move News digest to 19:00": the card heading of a snooze, in kernel text.
pub fn move_to(title: &str, when: &str) -> String {
    format!("Move {title} to {when}")
}

/// The title in a heading [`move_to`] wrote: "News digest" of "Move News digest to 19:00".
/// The time is "19:00" or "Tue 19:00", so the last " to " ends the title.
pub(crate) fn title_of_move(heading: &str) -> Option<&str> {
    let (title, _when) = heading.strip_prefix("Move ")?.rsplit_once(" to ")?;
    (!title.is_empty()).then_some(title)
}

/// "Moved News digest to 19:00".
pub fn moved_to(title: &str, when: &str) -> String {
    format!("Moved {title} to {when}")
}

/// "snoozed News digest → 19:00": the terminal's line once it ran.
pub fn snoozed(title: &str, when: &str) -> String {
    format!("snoozed {title} → {when}")
}

/// "Can't snooze: it has already started".
pub fn cant_snooze(why: &str) -> String {
    format!("Can't snooze: {why}")
}

/// "It can only move to 19:00 or Tue 18:00": refused, a time that is neither of an item's two.
pub fn only_moves_to(times: &[String]) -> String {
    match times {
        [one] => format!("It can only move to {one}"),
        [first, second] => format!("It can only move to {first} or {second}"),
        _ => format!("It can only move to {}", times.join(", ")),
    }
}

/// "It ends at 18:30, so it can't move past then".
pub fn ends_at(when: &str) -> String {
    format!("It ends at {when}, so it can't move past then")
}

/// "Finance already has 3 changes waiting for you".
pub fn already_waiting(agent: &str, count: usize) -> String {
    format!("{agent} already has {count} changes waiting for you")
}

/// "nothing in Coming up is called “laundry”".
pub fn nothing_called(title: &str) -> String {
    format!("nothing in Coming up is called “{title}”")
}

/// "Two things match “digest”": the start of an ambiguous match.
pub fn things_match(count: usize, said: &str) -> String {
    let count = match count {
        2 => "Two".to_string(),
        3 => "Three".to_string(),
        4 => "Four".to_string(),
        other => other.to_string(),
    };
    format!("{count} things match “{said}”")
}

/// "Snoozing in 5 s": the voice card's pill.
pub fn snoozing_in(seconds: u64) -> String {
    format!("Snoozing in {seconds} s")
}

/// "Heard “snooze news digest”".
pub fn heard(said: &str) -> String {
    format!("Heard “{said}”")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A heading's title reads back as it was written, " to " in a title included.
    #[test]
    fn the_title_of_a_move_reads_back() {
        for (title, when) in
            [("News digest", "19:00"), ("Drive to work", "Tue 07:30"), ("to do", "08:00")]
        {
            assert_eq!(title_of_move(&move_to(title, when)), Some(title));
        }
        assert_eq!(title_of_move("Snooze something in Coming up"), None);
        assert_eq!(title_of_move("Move  to 19:00"), None);
    }
}
