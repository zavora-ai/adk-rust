//! Long-term memory catching up, as a REPL starts, on the earlier sessions that ended without
//! being distilled (spec 3.1 §7.33 fix 9): a session killed mid-run, or left before its idle
//! distillation, otherwise teaches long-term memory nothing.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use adk_core::{Result, UserId};
use adk_harness::Harness;
use adk_session::{GetRequest, ListRequest, SessionService};
use tokio::task::JoinHandle;

use super::launcher::{Shared, WORKSPACE_STATE_KEY, memory_on};
use crate::console::ConsoleIo;

/// Most earlier sessions one start distils.
const MOST_SESSIONS: usize = 5;

/// How many of the user's most recently updated sessions one start reads.
const MOST_READ: usize = 50;

/// Starts distilling, in the background, the earlier sessions of `user` recorded for
/// `workspace` that ended without being distilled into long-term memory (spec 3.1 §7.33
/// fix 9); the session `current` is left to the harness's own distiller.
///
/// Of the user's 50 most recently updated sessions, those whose distillation cursor stops
/// short of their last event are offered to [`Harness::distill_sessions`] most recent first,
/// until five are distilled. That entry decides what is skipped — a session active within
/// the distiller's idle window, which another terminal may still be working in, and one in
/// ReadOnly or Plan mode — and stores every fact `Assumed`, each session under the
/// session-end distillation's budget. The task ends with the notice to show, such as "3
/// facts from 2 earlier sessions that ended without saving were added as assumed; /memory
/// lists them", or `None` when no fact was added; a failure is logged and ends it early.
/// Returns `None` when the harness keeps no long-term facts.
///
/// # Example
///
/// ```rust,ignore
/// use adk_cli::harness::distill_unsaved_sessions;
///
/// let catch_up =
///     distill_unsaved_sessions(harness, sessions, "adk-rust", &user, &workspace, &session_id);
/// if let Some(task) = catch_up
///     && let Ok(Some(notice)) = task.await
/// {
///     println!("{notice}");
/// }
/// ```
pub fn distill_unsaved_sessions(
    harness: Arc<Harness>,
    sessions: Arc<dyn SessionService>,
    app_name: &str,
    user: &UserId,
    workspace: &Path,
    current: &str,
) -> Option<JoinHandle<Option<String>>> {
    if !memory_on(&harness) {
        return None;
    }
    let (app_name, user, current) = (app_name.to_string(), user.clone(), current.to_string());
    let workspace = workspace.display().to_string();
    Some(tokio::spawn(async move {
        let mut caught = Caught::default();
        if let Err(error) = catch_up(
            &harness,
            sessions.as_ref(),
            &app_name,
            &user,
            &workspace,
            &current,
            &mut caught,
        )
        .await
        {
            tracing::warn!(error = %error, "distilling the sessions that ended without saving failed");
        }
        caught.notice()
    }))
}

/// What one start's catch-up added.
#[derive(Debug, Default, PartialEq)]
struct Caught {
    /// Sessions that added at least one fact.
    sessions: usize,
    /// Facts added.
    facts: usize,
}

impl Caught {
    fn notice(&self) -> Option<String> {
        if self.facts == 0 {
            return None;
        }
        let (facts, were) = if self.facts == 1 { ("fact", "was") } else { ("facts", "were") };
        let sessions = if self.sessions == 1 { "session" } else { "sessions" };
        Some(format!(
            "{} {facts} from {} earlier {sessions} that ended without saving {were} added as \
             assumed; /memory lists them",
            self.facts, self.sessions
        ))
    }
}

async fn catch_up(
    harness: &Harness,
    sessions: &dyn SessionService,
    app_name: &str,
    user: &UserId,
    workspace: &str,
    current: &str,
    caught: &mut Caught,
) -> Result<()> {
    let cursors: HashMap<String, chrono::DateTime<chrono::Utc>> =
        harness.distilled_sessions(user).await?.into_iter().collect();
    let mut listed = sessions
        .list(ListRequest {
            app_name: app_name.to_string(),
            user_id: user.to_string(),
            limit: None,
            offset: None,
        })
        .await?;
    listed.sort_by_key(|session| std::cmp::Reverse(session.last_update_time()));
    let mut distilled = 0;
    for id in listed.iter().map(|session| session.id()).filter(|id| *id != current).take(MOST_READ)
    {
        let request = GetRequest {
            app_name: app_name.to_string(),
            user_id: user.to_string(),
            session_id: id.to_string(),
            num_recent_events: Some(1),
            after: None,
        };
        // A session deleted since it was listed, or one unreadable, is skipped.
        let session = match sessions.get(request).await {
            Ok(session) => session,
            Err(error) => {
                tracing::debug!(error = %error, session.id = %id, "a session to catch up on is not readable");
                continue;
            }
        };
        let here = session
            .state()
            .get(WORKSPACE_STATE_KEY)
            .is_some_and(|recorded| recorded.as_str() == Some(workspace));
        let Some(last) = session.events().all().last().map(|event| event.timestamp) else {
            continue;
        };
        if !here || cursors.get(id).is_some_and(|upto| *upto >= last) {
            continue;
        }
        let report =
            harness.distill_sessions(user, &[id.to_string()], chrono::Utc::now(), None).await?;
        if report.sessions.is_empty() {
            continue;
        }
        if !report.stored.is_empty() {
            caught.sessions += 1;
            caught.facts += report.stored.len();
        }
        distilled += 1;
        if distilled == MOST_SESSIONS {
            break;
        }
    }
    Ok(())
}

/// The REPL's catch-up, whose notice is printed at the prompt after it finishes, since the
/// line console has no way to print over a prompt it is reading.
pub(crate) struct CatchUp(Option<JoinHandle<Option<String>>>);

impl CatchUp {
    /// Starts the catch-up for the session the REPL opened.
    pub(crate) fn start(shared: &Shared) -> Self {
        Self(distill_unsaved_sessions(
            shared.harness(),
            shared.sessions.clone(),
            &shared.app_name,
            &shared.user,
            &shared.workspace,
            shared.session.as_ref(),
        ))
    }

    /// Prints the notice once the catch-up has finished.
    pub(crate) async fn print_if_done(&mut self, console: &ConsoleIo) {
        if self.0.as_ref().is_some_and(JoinHandle::is_finished) {
            self.finish(console).await;
        }
    }

    /// Waits for the catch-up and prints its notice: the REPL ends after what it started.
    pub(crate) async fn finish(&mut self, console: &ConsoleIo) {
        if let Some(task) = self.0.take()
            && let Ok(Some(notice)) = task.await
        {
            console.print(&format!("{notice}\n\n"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Caught;

    #[test]
    fn the_notice_counts_facts_and_sessions_and_says_nothing_for_none() {
        assert_eq!(Caught::default().notice(), None);
        assert_eq!(
            Caught { sessions: 1, facts: 1 }.notice().as_deref(),
            Some(
                "1 fact from 1 earlier session that ended without saving was added as assumed; \
                 /memory lists them"
            )
        );
        assert_eq!(
            Caught { sessions: 2, facts: 3 }.notice().as_deref(),
            Some(
                "3 facts from 2 earlier sessions that ended without saving were added as \
                 assumed; /memory lists them"
            )
        );
    }
}
