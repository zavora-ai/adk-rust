//! Approval prompts at the console (spec 3.1 §8.1).

use adk_core::{
    Result, ToolApproval, ToolConfirmationDecision, ToolConfirmationHandler,
    ToolConfirmationRequest,
};
use async_trait::async_trait;
use serde_json::Value;

use super::{ConsoleInput, ConsoleIo, Edited, edit_json};
use crate::launcher::terminal_safe;

const MENU: &str = "  [y] allow once  [n] deny (reason)  [e] edit args  [?] details > ";
/// What [`ToolConfirmationHandler::decide`] can carry: approve or deny, nothing more.
const DECISION_MENU: &str = "  [y] allow once  [n] deny  [?] details > ";

/// Asks the person at the console to decide each tool call held for confirmation
/// (spec 3.1 §8.1).
///
/// The prompt shows `[approval needed] <tool> <args>` and offers
/// `[y] allow once  [n] deny (reason)  [e] edit args  [?] details`:
///
/// | Answer | Effect |
/// |--------|--------|
/// | `y` | Runs the call once, with the edited arguments when `e` changed them |
/// | `n` | Denies the call; a reason typed at the follow-up prompt is reported to the model |
/// | `e` | Replaces the arguments with JSON from `$VISUAL`/`$EDITOR`, or typed on one line, then asks again |
/// | `?` | Shows the call id, acting agent, tool origin and the full arguments |
/// | Ctrl-C | Denies the call and cancels the run ([`ConsoleIo::interrupt_run`]) |
///
/// Prompts are serialized: calls decided concurrently in one batch are asked one after
/// another. Without a terminal ([`ConsoleIo::is_terminal`]) every call is denied with a
/// message saying so, so an unattended run never waits for input. Once the run is
/// interrupted, calls still queued are denied without a prompt.
///
/// The governed path asks through
/// [`decide_approval`](ToolConfirmationHandler::decide_approval), which offers every answer
/// and returns the edited arguments and the denial reason in the [`ToolApproval`]. A direct
/// call to [`decide`](ToolConfirmationHandler::decide), which returns only approve or deny,
/// offers `[y] allow once  [n] deny  [?] details`: an edit would run the original arguments
/// and a reason would never reach the model.
///
/// # Example
///
/// ```rust
/// use adk_cli::console::{ConsoleConfirmationHandler, ConsoleInput, ConsoleIo};
/// use adk_core::{ToolApproval, ToolConfirmationHandler, ToolConfirmationRequest};
/// use serde_json::json;
///
/// # futures::executor::block_on(async {
/// let console = ConsoleIo::scripted(
///     [ConsoleInput::Line("e".into()), ConsoleInput::Line(r#"{"path": "draft.txt"}"#.into()), ConsoleInput::Line("y".into())],
///     true,
/// );
/// let handler = ConsoleConfirmationHandler::new(console.clone());
/// let request = ToolConfirmationRequest::new("delete_file", json!({ "path": "notes.txt" }));
///
/// let approval = handler.decide_approval(&request).await?;
/// assert_eq!(approval, ToolApproval::approve().with_edited_args(json!({ "path": "draft.txt" })));
/// assert!(console.transcript().contains("[approval needed] delete_file"));
/// # Ok::<(), adk_core::AdkError>(())
/// # }).unwrap();
/// ```
#[derive(Debug)]
pub struct ConsoleConfirmationHandler {
    console: ConsoleIo,
    turn: tokio::sync::Mutex<()>,
}

impl ConsoleConfirmationHandler {
    /// A handler that prompts on `console`.
    pub fn new(console: ConsoleIo) -> Self {
        Self { console, turn: tokio::sync::Mutex::new(()) }
    }

    /// `full` offers editing and a denial reason, which only a [`ToolApproval`] carries.
    async fn prompt(&self, request: &ToolConfirmationRequest, full: bool) -> Result<ToolApproval> {
        let _turn = self.turn.lock().await;
        let console = &self.console;
        if console.run_interrupted() {
            return Ok(ToolApproval::deny().with_message("the run was interrupted"));
        }
        if !console.is_terminal() {
            console.print(&format!(
                "\n[approval needed] {} {} — denied: there is no terminal to ask. Run \
                 interactively to approve it, or decide it with a tool policy.\n",
                terminal_safe(&request.tool_name),
                terminal_safe(&request.args.to_string()),
            ));
            return Ok(
                ToolApproval::deny().with_message("no terminal was available to ask for approval")
            );
        }

        let mut args = request.args.clone();
        console.print(&format!(
            "\n[approval needed] {} {}\n",
            terminal_safe(&request.tool_name),
            terminal_safe(&args.to_string()),
        ));
        let menu = if full { MENU } else { DECISION_MENU };
        loop {
            let answer = match console.read_line(menu).await {
                ConsoleInput::Line(answer) => answer,
                ConsoleInput::Interrupted => return Ok(self.interrupted()),
                ConsoleInput::Eof => {
                    return Ok(ToolApproval::deny().with_message("input ended at the prompt"));
                }
            };
            match answer.trim() {
                "y" | "yes" => {
                    let approval = ToolApproval::approve();
                    return Ok(if args == request.args {
                        approval
                    } else {
                        approval.with_edited_args(args)
                    });
                }
                "n" | "no" if !full => return Ok(ToolApproval::deny()),
                "n" | "no" => {
                    return Ok(match console.read_line("  reason (optional) > ").await {
                        ConsoleInput::Interrupted => self.interrupted(),
                        ConsoleInput::Line(reason) if !reason.trim().is_empty() => {
                            ToolApproval::deny().with_message(reason.trim())
                        }
                        ConsoleInput::Line(_) | ConsoleInput::Eof => ToolApproval::deny(),
                    });
                }
                "e" | "edit" if full => match edit_json(console, &args).await {
                    Edited::Args(edited) => {
                        args = edited;
                        console.print(&format!(
                            "  edited: {} {}\n",
                            terminal_safe(&request.tool_name),
                            terminal_safe(&args.to_string()),
                        ));
                    }
                    Edited::Unchanged(note) => console.print(&format!("  {note}\n")),
                    Edited::Interrupted => return Ok(self.interrupted()),
                },
                "?" | "details" => console.print(&details(request, &args)),
                _ => {}
            }
        }
    }

    fn interrupted(&self) -> ToolApproval {
        self.console.interrupt_run();
        self.console.print("  denied; the run is interrupted\n");
        ToolApproval::deny().with_message("interrupted at the approval prompt")
    }
}

#[async_trait]
impl ToolConfirmationHandler for ConsoleConfirmationHandler {
    async fn decide(&self, request: &ToolConfirmationRequest) -> Result<ToolConfirmationDecision> {
        Ok(self.prompt(request, false).await?.decision)
    }

    /// Offers every answer; edited arguments and a denial reason travel in the approval.
    async fn decide_approval(&self, request: &ToolConfirmationRequest) -> Result<ToolApproval> {
        self.prompt(request, true).await
    }
}

fn details(request: &ToolConfirmationRequest, args: &Value) -> String {
    let mut out = format!("  tool      : {}\n", terminal_safe(&request.tool_name));
    if let Some(id) = &request.function_call_id {
        out.push_str(&format!("  call id   : {}\n", terminal_safe(id)));
    }
    if let Some(acting) = &request.acting {
        out.push_str(&format!("  agent     : {}\n", terminal_safe(acting.agent.id.as_str())));
        if let Some(subject) = &acting.on_behalf_of {
            let kind = format!("{:?}", subject.kind).to_lowercase();
            out.push_str(&format!("  for       : {kind} {}\n", terminal_safe(&subject.id)));
        }
    }
    if let Some(origin) = &request.origin {
        out.push_str(&format!(
            "  origin    : {} ({})\n",
            terminal_safe(&origin.source),
            terminal_safe(&origin.declaration_digest)
        ));
    }
    let pretty = serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string());
    out.push_str("  arguments :\n");
    for line in pretty.lines() {
        out.push_str(&format!("    {}\n", terminal_safe(line)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::principal::{ActingContext, AgentPrincipal, SubjectRef};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    fn request() -> ToolConfirmationRequest {
        ToolConfirmationRequest::new("record_note", json!({ "note": "ship it" }))
            .with_function_call_id("call-1")
    }

    fn line(text: &str) -> ConsoleInput {
        ConsoleInput::Line(text.into())
    }

    fn scripted(
        inputs: impl IntoIterator<Item = ConsoleInput>,
    ) -> (ConsoleConfirmationHandler, ConsoleIo) {
        let console = ConsoleIo::scripted(inputs, true);
        (ConsoleConfirmationHandler::new(console.clone()), console)
    }

    #[tokio::test]
    async fn y_allows_the_call_once() {
        let (handler, console) = scripted([line("y")]);
        assert_eq!(handler.decide_approval(&request()).await.unwrap(), ToolApproval::approve());
        let shown = console.transcript();
        assert!(shown.contains(r#"[approval needed] record_note {"note":"ship it"}"#), "{shown}");
        assert!(shown.contains(MENU), "{shown}");
    }

    #[tokio::test]
    async fn n_denies_with_the_reason_typed() {
        let (handler, _) = scripted([line("n"), line("  wrong file  ")]);
        assert_eq!(
            handler.decide_approval(&request()).await.unwrap(),
            ToolApproval::deny().with_message("wrong file")
        );

        let (handler, _) = scripted([line("no"), line("")]);
        assert_eq!(handler.decide_approval(&request()).await.unwrap(), ToolApproval::deny());
    }

    #[tokio::test]
    async fn e_replaces_the_arguments_and_y_runs_the_edited_call() {
        let (handler, console) =
            scripted([line("e"), line(r#"{"note": "ship it tomorrow"}"#), line("y")]);
        assert_eq!(
            handler.decide_approval(&request()).await.unwrap(),
            ToolApproval::approve().with_edited_args(json!({ "note": "ship it tomorrow" }))
        );
        assert!(
            console.transcript().contains(r#"edited: record_note {"note":"ship it tomorrow"}"#)
        );
    }

    #[tokio::test]
    async fn invalid_json_keeps_the_original_arguments() {
        let (handler, console) = scripted([line("e"), line("{not json"), line("y")]);
        assert_eq!(handler.decide_approval(&request()).await.unwrap(), ToolApproval::approve());
        assert!(console.transcript().contains("not valid JSON"));
    }

    #[tokio::test]
    async fn decide_offers_only_what_a_decision_carries() {
        // An edit would run the original arguments, and a reason would never reach the model.
        let (handler, console) = scripted([line("e"), line("y")]);
        assert_eq!(handler.decide(&request()).await.unwrap(), ToolConfirmationDecision::Approve);
        let (denier, denied) = scripted([line("n"), line("y")]);
        assert_eq!(denier.decide(&request()).await.unwrap(), ToolConfirmationDecision::Deny);
        for shown in [console.transcript(), denied.transcript()] {
            assert!(shown.contains(DECISION_MENU), "{shown}");
            assert!(!shown.contains("arguments (JSON)"), "{shown}");
            assert!(!shown.contains("reason (optional)"), "{shown}");
        }
    }

    #[tokio::test]
    async fn ctrl_c_denies_and_cancels_the_run() {
        let (handler, console) = scripted([ConsoleInput::Interrupted, line("y")]);
        let run = CancellationToken::new();
        console.set_run(Some(run.clone()));

        let approval = handler.decide_approval(&request()).await.unwrap();
        assert_eq!(approval.decision, ToolConfirmationDecision::Deny);
        assert!(run.is_cancelled());

        // A call still queued in the same batch is denied without a prompt.
        let shown = console.transcript();
        assert_eq!(handler.decide(&request()).await.unwrap(), ToolConfirmationDecision::Deny);
        assert_eq!(console.transcript(), shown);
    }

    #[tokio::test]
    async fn without_a_terminal_every_call_is_denied_without_reading() {
        let console = ConsoleIo::scripted([line("y")], false);
        let handler = ConsoleConfirmationHandler::new(console.clone());
        assert_eq!(handler.decide(&request()).await.unwrap(), ToolConfirmationDecision::Deny);
        assert!(console.transcript().contains("there is no terminal to ask"));
        // The scripted "y" was never consumed.
        assert_eq!(console.read_line("").await, line("y"));
    }

    #[tokio::test]
    async fn details_show_who_acts_and_the_tool_origin() {
        let acting = ActingContext::new(AgentPrincipal::derived("adk-rust", "adk_agent"))
            .on_behalf_of(SubjectRef::human("ana"));
        let request = request().with_acting_context(acting).with_origin(adk_core::ToolOrigin::new(
            "static",
            &json!({}),
            None,
        ));
        let (handler, console) = scripted([line("?"), line("y")]);
        handler.decide_approval(&request).await.unwrap();
        let shown = console.transcript();
        for expected in [
            "call id   : call-1",
            "agent     : urn:adk:agent:adk-rust/adk_agent",
            "for       : human ana",
            "origin    : static (sha256:",
            "\"note\": \"ship it\"",
        ] {
            assert!(shown.contains(expected), "missing {expected:?} in {shown}");
        }
    }

    #[tokio::test]
    async fn planted_escape_sequences_are_shown_escaped() {
        let request =
            ToolConfirmationRequest::new("tool\x1b]52;c;x\x07", json!({ "q": "\x1b[2J" }));
        let (handler, console) = scripted([line("?"), line("n"), line("")]);
        handler.decide_approval(&request).await.unwrap();
        let shown = console.transcript();
        assert!(!shown.contains('\x1b'), "{shown:?}");
    }
}
