//! Slash commands registered with the console (spec 3.1 §8.1).

use std::sync::Arc;

use adk_core::Result;
use adk_session::SessionService;
use async_trait::async_trait;

use super::ConsoleIo;

/// A slash command the console REPL runs instead of sending the line to the agent
/// (spec 3.1 §8.1).
///
/// Register commands with [`Launcher::with_console_command`](crate::Launcher::with_console_command).
/// The REPL runs `/name args` through the command whose [`name`](Self::name) matches and
/// prints what it returns, with terminal control characters escaped; `/help` lists every
/// registered command with its [`description`](Self::description). The built-in `/help`,
/// `/clear`, `/quit` and `/exit` take precedence over a command with the same name. A
/// slash line no command matches is sent to the agent, as before.
///
/// # Example
///
/// ```rust
/// use adk_cli::console::{ConsoleCommand, ConsoleCommandContext};
/// use async_trait::async_trait;
///
/// struct WhoAmI;
///
/// #[async_trait]
/// impl ConsoleCommand for WhoAmI {
///     fn name(&self) -> &str {
///         "whoami"
///     }
///
///     fn description(&self) -> &str {
///         "Show the user and session of this console"
///     }
///
///     async fn run(&self, _args: &str, ctx: &ConsoleCommandContext) -> adk_core::Result<String> {
///         Ok(format!("{} in session {}", ctx.user_id, ctx.session_id))
///     }
/// }
/// ```
#[async_trait]
pub trait ConsoleCommand: Send + Sync {
    /// The command's name, typed after the slash: `mode` for `/mode`.
    fn name(&self) -> &str;

    /// One line shown next to the command by `/help`.
    fn description(&self) -> &str;

    /// Runs the command with the text after its name, trimmed, and returns what to print.
    ///
    /// # Errors
    ///
    /// An error is printed and the REPL continues.
    async fn run(&self, args: &str, ctx: &ConsoleCommandContext) -> Result<String>;
}

/// What a [`ConsoleCommand`] runs against: the console's session and its terminal
/// (spec 3.1 §8.1).
#[non_exhaustive]
#[derive(Clone)]
pub struct ConsoleCommandContext {
    /// The application name runs use.
    pub app_name: String,
    /// The user the console runs as.
    pub user_id: String,
    /// The session the console continues.
    pub session_id: String,
    /// The service holding the session.
    pub session_service: Arc<dyn SessionService>,
    /// The console, for commands that ask follow-up questions.
    pub console: ConsoleIo,
}

impl ConsoleCommandContext {
    /// A context for the session `session_id` of `user_id` in `app_name`.
    pub fn new(
        app_name: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
        session_service: Arc<dyn SessionService>,
        console: ConsoleIo,
    ) -> Self {
        Self {
            app_name: app_name.into(),
            user_id: user_id.into(),
            session_id: session_id.into(),
            session_service,
            console,
        }
    }
}
