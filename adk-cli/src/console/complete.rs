//! Tab completion at the console (spec 3.1 §7.21, §8.1): `@server:uri` resource mentions
//! anywhere in a line, and slash commands at its start.

use std::sync::{Arc, Mutex, PoisonError};

use rustyline::Context;
use rustyline::completion::{Completer, Pair};
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;

/// The words the console completes, replaced as a whole by the surface that knows them.
#[derive(Debug, Default)]
pub(crate) struct Completions {
    /// Slash commands with their slash, offered at the start of a line.
    commands: Mutex<Vec<String>>,
    /// `@server:uri` mentions, offered anywhere in a line.
    mentions: Mutex<Vec<String>>,
}

impl Completions {
    pub(crate) fn set_commands(&self, commands: Vec<String>) {
        *lock(&self.commands) = commands;
    }

    pub(crate) fn set_mentions(&self, mentions: Vec<String>) {
        *lock(&self.mentions) = mentions;
    }

    /// The start of the word the cursor at `pos` is in, and the candidates that extend
    /// it: mentions for a word starting with `@`, commands for a line starting with `/`.
    /// A word that is already a candidate, or that is neither, completes to nothing.
    pub(crate) fn complete(&self, line: &str, pos: usize) -> (usize, Vec<String>) {
        let pos = pos.min(line.len());
        if !line.is_char_boundary(pos) {
            return (pos, Vec::new());
        }
        let before = &line[..pos];
        let start = before.rfind(char::is_whitespace).map_or(0, |at| at + 1);
        let word = &before[start..];
        let candidates = if word.starts_with('@') {
            lock(&self.mentions)
        } else if start == 0 && word.starts_with('/') {
            lock(&self.commands)
        } else {
            return (pos, Vec::new());
        };
        let matching: Vec<String> = candidates
            .iter()
            .filter(|candidate| candidate.starts_with(word) && candidate.as_str() != word)
            .cloned()
            .collect();
        (start, matching)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The editor helper behind the terminal console: completion from [`Completions`], no
/// hints, highlighting or validation.
pub(crate) struct ConsoleHelper {
    completions: Arc<Completions>,
}

impl ConsoleHelper {
    pub(crate) fn new(completions: Arc<Completions>) -> Self {
        Self { completions }
    }
}

impl Completer for ConsoleHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let (start, words) = self.completions.complete(line, pos);
        // A completed word is whole, so the next keystroke starts the next one.
        let pairs = words
            .into_iter()
            .map(|word| Pair { replacement: format!("{word} "), display: word })
            .collect();
        Ok((start, pairs))
    }
}

impl Hinter for ConsoleHelper {
    type Hint = String;
}

impl Highlighter for ConsoleHelper {}

impl Validator for ConsoleHelper {}

impl rustyline::Helper for ConsoleHelper {}

#[cfg(test)]
mod tests {
    use super::*;

    fn completions() -> Completions {
        let completions = Completions::default();
        completions.set_commands(vec!["/mode".into(), "/mcp".into(), "/memory".into()]);
        completions.set_mentions(vec![
            "@docs:file:///guide.md".into(),
            "@docs:file:///notes.md".into(),
            "@fixture:memo://1".into(),
        ]);
        completions
    }

    #[test]
    fn a_mention_completes_from_its_server_prefix_anywhere_in_the_line() {
        let completions = completions();
        let line = "summarise @docs:";
        assert_eq!(
            completions.complete(line, line.len()),
            (10, vec!["@docs:file:///guide.md".into(), "@docs:file:///notes.md".into()])
        );
        assert_eq!(completions.complete("@fix", 4), (0, vec!["@fixture:memo://1".into()]));
        // The cursor in the middle of a line completes the word it is in.
        assert_eq!(completions.complete("@fix and more", 4), (0, vec!["@fixture:memo://1".into()]));
    }

    #[test]
    fn a_slash_command_completes_at_the_start_of_the_line_only() {
        let completions = completions();
        assert_eq!(completions.complete("/mo", 3), (0, vec!["/mode".into()]));
        assert_eq!(
            completions.complete("/m", 2),
            (0, vec!["/mode".into(), "/mcp".into(), "/memory".into()])
        );
        assert_eq!(completions.complete("say /mo", 7), (7, Vec::<String>::new()));
    }

    #[test]
    fn plain_words_and_complete_candidates_offer_nothing() {
        let completions = completions();
        assert_eq!(completions.complete("hello", 5), (5, Vec::<String>::new()));
        assert_eq!(completions.complete("/mode", 5), (0, Vec::<String>::new()));
        assert_eq!(completions.complete("@nope:", 6), (0, Vec::<String>::new()));
        // A position inside a multi-byte character completes nothing rather than panicking.
        assert_eq!(completions.complete("@é", 2), (2, Vec::<String>::new()));
    }

    #[test]
    fn the_helper_appends_a_space_to_the_elected_word() {
        let helper = ConsoleHelper::new(Arc::new(completions()));
        let history = rustyline::history::DefaultHistory::new();
        let context = Context::new(&history);
        let (start, pairs) = helper.complete("/mo", 3, &context).unwrap();
        assert_eq!(start, 0);
        assert_eq!(pairs.len(), 1);
        assert_eq!((pairs[0].display.as_str(), pairs[0].replacement.as_str()), ("/mode", "/mode "));
    }
}
