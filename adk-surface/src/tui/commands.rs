//! Slash commands an embedder adds to the terminal, and completing their names in the message
//! box (spec 3.3 §5.2).

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;

/// A slash command the terminal runs for its embedder: `/name args` in the message box runs
/// it, and what it returns is shown in a pane (spec 3.3 §5.2).
///
/// `adk-rust` adds the harness REPL's commands this way (`/todos`, `/rules`, `/memory`,
/// `/hooks`, ...), so the full screen runs the same code as the REPL, in process or on a
/// kernel. The terminal's own commands (`/help`, `/today`, `/permissions`, `/mode`,
/// `/session`, `/stop`, `/resume`, `/quit`, `/exit`) take precedence over an added command of
/// the same name; an added `mode` or `session` answers the bare `/mode` and `/session`, which
/// show the mode and the session. What a command returns is shown as plain text, every
/// control character escaped (S-I9).
///
/// # Example
///
/// ```rust
/// use adk_surface::tui::{CommandContext, TerminalCommand};
/// use async_trait::async_trait;
///
/// struct Uptime;
///
/// #[async_trait]
/// impl TerminalCommand for Uptime {
///     fn name(&self) -> &str {
///         "uptime"
///     }
///
///     fn description(&self) -> &str {
///         "How long the kernel has run"
///     }
///
///     async fn run(&self, _args: &str, context: &CommandContext) -> adk_core::Result<String> {
///         Ok(format!("Up 3 hours; this thread is {}", context.session_id))
///     }
/// }
/// ```
#[async_trait]
pub trait TerminalCommand: Send + Sync {
    /// The name typed after the slash: `todos` for `/todos`.
    fn name(&self) -> &str;

    /// One line shown next to the name while completing and in `/help`.
    fn description(&self) -> &str;

    /// Runs the command with the text after its name, trimmed, in the session the terminal
    /// shows, and returns what to show.
    ///
    /// # Errors
    ///
    /// An error is shown in the pane in place of the output, and the terminal carries on.
    async fn run(&self, args: &str, context: &CommandContext) -> adk_core::Result<String>;
}

/// Where a [`TerminalCommand`] runs: the session whose thread the terminal shows (spec 3.3
/// §5.2), which changes with `Tab` and `/session`.
///
/// # Example
///
/// ```rust
/// use adk_surface::tui::CommandContext;
///
/// assert_eq!(CommandContext::new("s1").session_id, "s1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CommandContext {
    /// The session the terminal shows.
    pub session_id: String,
}

impl CommandContext {
    /// The context of a command run while `session_id` is shown.
    pub fn new(session_id: impl Into<String>) -> Self {
        Self { session_id: session_id.into() }
    }
}

/// The terminal's own commands, in `/help` order: they win over an added command of the
/// same name.
pub(crate) const BUILT_IN: &[(&str, &str)] = &[
    ("help", "Keys and commands"),
    ("today", "Today: while you were away"),
    ("permissions", "Permissions: what your agents can do"),
    ("agents", "Your agents"),
    ("mode", "Show or set the mode: just look, suggest, ask first or go ahead"),
    ("session", "This session; /session new or /session ID opens another"),
    ("stop", "Stop all"),
    ("resume", "Resume all"),
    ("quit", "Leave; a kernel's agents carry on"),
    ("exit", "Leave; a kernel's agents carry on"),
];

/// The commands the terminal offers: its own, then the embedder's.
#[derive(Clone, Default)]
pub(crate) struct Commands {
    added: Vec<Arc<dyn TerminalCommand>>,
    /// Added commands named as one of the terminal's own: they answer only its bare form.
    shadowed: Vec<Arc<dyn TerminalCommand>>,
}

impl fmt::Debug for Commands {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.added.iter().map(|command| command.name())).finish()
    }
}

impl Commands {
    /// Adds `command`: the first one of a name stays, and one named as the terminal's own
    /// answers only that command's bare form.
    pub(crate) fn add(&mut self, command: Arc<dyn TerminalCommand>) {
        let name = command.name();
        if self.added.iter().chain(&self.shadowed).any(|added| added.name() == name) {
            return;
        }
        if BUILT_IN.iter().any(|(built_in, _)| *built_in == name) {
            self.shadowed.push(command);
        } else {
            self.added.push(command);
        }
    }

    /// The added command `name`, if there is one.
    pub(crate) fn find(&self, name: &str) -> Option<Arc<dyn TerminalCommand>> {
        self.added.iter().find(|command| command.name() == name).cloned()
    }

    /// The added command named as the terminal's own `name`, which answers its bare form.
    pub(crate) fn shadowed(&self, name: &str) -> Option<Arc<dyn TerminalCommand>> {
        self.shadowed.iter().find(|command| command.name() == name).cloned()
    }

    /// The added commands' names and descriptions, in the order they were added.
    pub(crate) fn added(&self) -> Vec<(String, String)> {
        self.added
            .iter()
            .map(|command| (command.name().to_string(), command.description().to_string()))
            .collect()
    }

    /// Every command whose name starts with `typed`, ignoring case: the terminal's own first,
    /// then the added ones.
    pub(crate) fn matching(&self, typed: &str) -> Vec<(String, String)> {
        let typed = typed.to_lowercase();
        BUILT_IN
            .iter()
            .map(|(name, description)| ((*name).to_string(), (*description).to_string()))
            .chain(self.added())
            .filter(|(name, _)| name.to_lowercase().starts_with(&typed))
            .collect()
    }
}

/// Completing a command's name while the message box holds `/` and the start of a name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Completion {
    /// The name typed so far, without the slash.
    pub typed: String,
    /// The commands it matches, with their descriptions.
    pub candidates: Vec<(String, String)>,
    /// The candidate the arrows have chosen.
    pub selected: usize,
}

impl Completion {
    /// The completion for `text` in the message box: only while it is `/` and a name with no
    /// space after it.
    pub(crate) fn of(text: &str, commands: &Commands) -> Option<Self> {
        let typed = text.strip_prefix('/')?;
        if typed.contains(char::is_whitespace) {
            return None;
        }
        Some(Self { typed: typed.to_string(), candidates: commands.matching(typed), selected: 0 })
    }

    /// The chosen candidate's name.
    pub(crate) fn chosen(&self) -> Option<&str> {
        self.candidates.get(self.selected).map(|(name, _)| name.as_str())
    }

    /// Whether what was typed is a whole command's name.
    pub(crate) fn whole(&self) -> bool {
        self.candidates.iter().any(|(name, _)| *name == self.typed)
    }

    /// Moves the choice by `step`, wrapping around.
    pub(crate) fn step(&mut self, step: isize) {
        let count = self.candidates.len();
        if count == 0 {
            return;
        }
        let at = self.selected as isize + step;
        self.selected = at.rem_euclid(count as isize) as usize;
    }

    /// What Tab puts in the message box: the only candidate, or the longest start every
    /// candidate shares when it is longer than what was typed, else the chosen candidate; a
    /// whole name gets a space after it, ready for its arguments.
    pub(crate) fn accept(&self) -> Option<String> {
        let names: Vec<&str> = self.candidates.iter().map(|(name, _)| name.as_str()).collect();
        match names.as_slice() {
            [] => None,
            [only] => Some(format!("/{only} ")),
            _ => {
                let shared = shared_start(&names);
                if shared.chars().count() > self.typed.chars().count() {
                    Some(format!("/{shared}"))
                } else {
                    names.get(self.selected).map(|name| format!("/{name} "))
                }
            }
        }
    }
}

/// The longest start all of `names` share.
fn shared_start(names: &[&str]) -> String {
    let Some((first, rest)) = names.split_first() else {
        return String::new();
    };
    let mut end = first.len();
    for name in rest {
        end = first
            .char_indices()
            .zip(name.chars())
            .take_while(|((_, a), b)| a == b)
            .last()
            .map_or(0, |((index, a), _)| index + a.len_utf8())
            .min(end);
    }
    first[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Named(&'static str, &'static str);

    #[async_trait]
    impl TerminalCommand for Named {
        fn name(&self) -> &str {
            self.0
        }

        fn description(&self) -> &str {
            self.1
        }

        async fn run(&self, args: &str, _: &CommandContext) -> adk_core::Result<String> {
            Ok(format!("{} {args}", self.0))
        }
    }

    fn commands(names: &[(&'static str, &'static str)]) -> Commands {
        let mut commands = Commands::default();
        for (name, description) in names {
            commands.add(Arc::new(Named(name, description)));
        }
        commands
    }

    #[test]
    fn the_terminals_own_commands_and_the_first_added_win() {
        let commands = commands(&[
            ("mode", "The REPL's mode"),
            ("todos", "The todo list"),
            ("todos", "A second todo list"),
        ]);
        assert_eq!(commands.added(), vec![("todos".to_string(), "The todo list".to_string())]);
        assert!(commands.find("mode").is_none());
        assert!(commands.find("todos").is_some());
        // The REPL's `/mode` answers the bare `/mode` only.
        assert_eq!(
            commands.shadowed("mode").map(|command| command.description().to_string()),
            Some("The REPL's mode".into())
        );
    }

    #[test]
    fn completion_lists_the_own_commands_first_then_the_added_ones() {
        let commands = commands(&[("memory", "Notes and facts"), ("mcp", "MCP servers")]);
        let completion = Completion::of("/m", &commands).unwrap();
        let names: Vec<&str> =
            completion.candidates.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["mode", "memory", "mcp"]);
        assert!(Completion::of("/mode ask", &commands).is_none());
        assert!(Completion::of("mode", &commands).is_none());
        assert_eq!(completion.chosen(), Some("mode"));
        assert!(!completion.whole());
        assert!(Completion::of("/mcp", &commands).unwrap().whole());
    }

    #[test]
    fn tab_takes_the_only_name_the_shared_start_or_the_chosen_name() {
        let commands = commands(&[("memory", "Notes and facts"), ("mcp", "MCP servers")]);
        assert_eq!(
            Completion::of("/mem", &commands).unwrap().accept().as_deref(),
            Some("/memory ")
        );
        assert_eq!(Completion::of("/me", &commands).unwrap().accept().as_deref(), Some("/memory "));
        let mut several = Completion::of("/m", &commands).unwrap();
        assert_eq!(several.accept().as_deref(), Some("/mode "));
        several.step(1);
        assert_eq!(several.accept().as_deref(), Some("/memory "));
        several.step(-2);
        assert_eq!(several.accept().as_deref(), Some("/mcp "));
        assert_eq!(Completion::of("/zz", &commands).unwrap().accept(), None);
        let shared = commands_with_shared_start();
        // "resume" shares only "r" with them, so Tab takes the chosen name.
        assert_eq!(Completion::of("/r", &shared).unwrap().accept().as_deref(), Some("/resume "));
        assert_eq!(Completion::of("/ru", &shared).unwrap().accept().as_deref(), Some("/rule"));
    }

    fn commands_with_shared_start() -> Commands {
        commands(&[("rules", "Standing rules"), ("rulebook", "The rulebook")])
    }

    #[test]
    fn the_shared_start_respects_characters_wider_than_a_byte() {
        assert_eq!(shared_start(&["café", "cafés"]), "café");
        assert_eq!(shared_start(&["éa", "éb"]), "é");
        assert_eq!(shared_start(&["a", "b"]), "");
    }
}
