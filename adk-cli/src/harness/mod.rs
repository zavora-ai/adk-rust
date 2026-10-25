//! The `adk-rust` harness: [`HarnessAgent`](adk_harness::HarnessAgent) at the console and
//! behind the server (spec 3.1 §8.1, §8.2; feature `harness`).
//!
//! | Item | Spec |
//! |---|---|
//! | [`HarnessArgs`], [`ResolvedHarnessArgs`] | §8.1 shared flags and their precedence |
//! | [`ConsolePrompter`] | §8.1, §7.5 approvals at the terminal, `[a] always allow` |
//! | [`HarnessLauncher`], [`TurnOutcome`] | §8.1 REPL, slash commands, one-turn runs, goals; §8.2 serve |
//! | [`LoopbackOperatorExtractor`], [`BearerTokenExtractor`] | §8.2 request identity |
//! | [`HarnessLauncher::with_kernel`], [`KernelSettings`] | spec 3.2 §8.1, §8.2: the resident form on the agent kernel and the `[kernel]` table |
//! | `/api/rules` on a served harness | §7.5, §8.2 standing rules over authenticated REST |
//! | [`ultracode`] | §7.10, §8.1 adaptive `ultracode`: the review router, the board, the limits |
//! | [`distill_unsaved_sessions`] | §7.33 fix 9: at start, long-term memory catches up on the sessions that ended without saving |
//!
//! # Example
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use adk_cli::console::ConsoleIo;
//! use adk_cli::harness::{ConsolePrompter, HarnessLauncher};
//! use adk_harness::HarnessAgent;
//!
//! async fn run(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<()> {
//!     let harness = HarnessAgent::builder("harness")
//!         .model(model)
//!         .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
//!         .prompter(Arc::new(ConsolePrompter::new(ConsoleIo::terminal()?)))
//!         .build()?;
//!     HarnessLauncher::new(harness).run().await
//! }
//! ```

mod agents;
mod args;
mod catch_up;
mod commands;
mod goal;
mod kernel;
mod launcher;
#[cfg(feature = "mobile")]
mod phone;
mod prompter;
mod rules;
mod serve;
pub mod ultracode;

pub use args::{
    HarnessArgs, MemberArg, MemberKindArg, PROJECT_CONFIG_FILE, ResolvedHarnessArgs, SearchArg,
};
pub use catch_up::distill_unsaved_sessions;
pub use commands::slash_command_entries;
pub use kernel::KernelSettings;
pub use launcher::{
    HarnessCommands, HarnessEffortRebuild, HarnessLauncher, HarnessRebuild, TurnOutcome,
};
#[cfg(feature = "mobile")]
pub use phone::PhoneSetup;
pub use prompter::ConsolePrompter;
pub use serve::{BearerTokenExtractor, LoopbackOperatorExtractor, OPERATOR_SCOPE};
