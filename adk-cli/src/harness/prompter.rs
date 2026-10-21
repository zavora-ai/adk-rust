//! Approval prompts for the harness at the console (spec 3.1 §8.1).

use adk_core::Result;
use adk_harness::{ApprovalPrompt, ApprovalPrompter, PromptAnswer, RuleSuggestion, ToolKind};
use async_trait::async_trait;
use serde_json::Value;

use crate::console::{ConsoleInput, ConsoleIo, edit_json};
use crate::launcher::terminal_safe;

const MENU: &str =
    "  [y] allow once  [a] always allow…  [n] deny (reason)  [e] edit args  [?] details > ";
/// The menu when no standing rule can be remembered for the call.
const MENU_ONCE: &str = "  [y] allow once  [n] deny (reason)  [e] edit args  [?] details > ";

/// Answers the harness's approval prompts at the console (spec 3.1 §8.1, §7.5).
///
/// It implements [`ApprovalPrompter`] over the shared [`ConsoleIo`] reader, so the REPL
/// and the prompts never compete for the terminal. Each held call is shown with its
/// class, mode, isolation, origin and acting agent, then
/// `[y] allow once  [a] always allow…  [n] deny (reason)  [e] edit args  [?] details`:
///
/// | Answer | Effect |
/// |--------|--------|
/// | `y` | Runs the call once, with the edited arguments when `e` changed them |
/// | `a` | Lists the standing rules this call can be remembered as; the chosen one is granted and the call runs |
/// | `n` | Denies the call; a reason typed at the follow-up prompt is reported to the model, and the turn ends once the denial is recorded (spec 3.1 §9.2 I19) |
/// | `e` | Replaces the arguments with JSON from `$VISUAL`/`$EDITOR`, or typed on one line, then asks again; the policy checks the edited call again |
/// | `?` | Shows the full call, untruncated |
/// | Ctrl-C | Denies this call and the rest of the turn, and cancels the run |
///
/// `[a]` appears only when the harness offers [`RuleSuggestion`]s for the call: never for
/// non-waivable calls such as `exit_plan_mode`, protected-path edits or unsandboxed
/// retries. A call whose arguments were cut for display can be approved only after `?`
/// has shown them in full (spec 3.1 §9.2 I21).
///
/// Without a terminal every call is denied with a message saying how to approve it, so
/// an unattended run never waits for input. [`approve_all`](Self::approve_all) is the
/// `--yes` behaviour.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
///
/// use adk_cli::console::ConsoleIo;
/// use adk_cli::harness::ConsolePrompter;
/// use adk_harness::HarnessAgent;
///
/// # fn build(model: Arc<dyn adk_core::Llm>) -> adk_core::Result<HarnessAgent> {
/// let prompter = ConsolePrompter::new(ConsoleIo::terminal()?);
/// HarnessAgent::builder("harness")
///     .model(model)
///     .workspace(adk_devtools::Workspace::new(".").allow_bash(true))
///     .prompter(Arc::new(prompter))
///     .build()
/// # }
/// ```
#[derive(Debug)]
pub struct ConsolePrompter {
    console: ConsoleIo,
    approve_all: Option<bool>,
}

impl ConsolePrompter {
    /// A prompter asking at `console`.
    pub fn new(console: ConsoleIo) -> Self {
        Self { console, approve_all: None }
    }

    /// Approves every call that reaches the prompter without asking: the `--yes` flag
    /// (spec 3.1 §8.1).
    ///
    /// Calls the policy denies never reach a prompter, so this never overrides a denial.
    /// An unsandboxed retry is approved only when `allow_unsandboxed` is true. Each
    /// approval is printed with the full call.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_cli::console::ConsoleIo;
    /// use adk_cli::harness::ConsolePrompter;
    ///
    /// let prompter = ConsolePrompter::new(ConsoleIo::scripted([], false)).approve_all(false);
    /// # let _ = prompter;
    /// ```
    #[must_use]
    pub fn approve_all(mut self, allow_unsandboxed: bool) -> Self {
        self.approve_all = Some(allow_unsandboxed);
        self
    }

    fn show(&self, prompt: &ApprovalPrompt) {
        let rendered = &prompt.rendered;
        let mut text = format!("\n[approval needed] {}\n", terminal_safe(&rendered.title));
        for line in rendered.body.lines() {
            text.push_str(&format!("  {}\n", terminal_safe(line)));
        }
        text.push_str(&format!(
            "  {} · mode {} · {} · origin {}\n",
            kind_label(&prompt.kind),
            prompt.mode,
            terminal_safe(&rendered.isolation),
            terminal_safe(&rendered.origin),
        ));
        text.push_str(&format!("  agent {}\n", terminal_safe(&rendered.agent)));
        if rendered.truncated {
            text.push_str("  (arguments cut for display; press ? to see them before approving)\n");
        }
        self.console.print(&text);
    }

    /// Prints a decision made without asking, after the call's line (spec 3.1 §8.1): the
    /// console holds it until its event reader has shown the call.
    fn print_decision(&self, prompt: &ApprovalPrompt, text: &str) {
        self.console.print_for_call(prompt.request.function_call_id.as_deref(), text);
    }

    fn approved_unattended(
        &self,
        prompt: &ApprovalPrompt,
        allow_unsandboxed: bool,
    ) -> PromptAnswer {
        let title = terminal_safe(&prompt.rendered.title).into_owned();
        if matches!(prompt.kind, ToolKind::ShellEscalation) && !allow_unsandboxed {
            self.print_decision(
                prompt,
                &format!(
                    "\n[denied] {title} — --yes does not approve an unsandboxed retry; add \
                     --allow-unsandboxed to approve it\n"
                ),
            );
            return PromptAnswer::Deny {
                message: Some(
                    "--yes does not approve unsandboxed retries without --allow-unsandboxed".into(),
                ),
            };
        }
        let mut text = format!("\n[approved by --yes] {title}\n");
        for line in prompt.rendered.expand().lines() {
            text.push_str(&format!("  {}\n", terminal_safe(line)));
        }
        self.print_decision(prompt, &text);
        PromptAnswer::ApproveOnce
    }

    fn deny_unattended(&self, prompt: &ApprovalPrompt) -> PromptAnswer {
        self.print_decision(
            prompt,
            &format!(
                "\n[approval needed] {} — denied: there is no terminal to ask. Run \
                 interactively, pass --yes to approve calls, or --mode plan to plan without \
                 changes.\n",
                terminal_safe(&prompt.rendered.title)
            ),
        );
        PromptAnswer::Deny { message: Some("no terminal was available to ask for approval".into()) }
    }

    fn interrupted(&self) -> PromptAnswer {
        self.console.interrupt_run();
        self.console.print("  denied; the run is interrupted\n");
        PromptAnswer::DenyAllRemaining
    }

    fn denied(&self, message: Option<String>) -> PromptAnswer {
        self.console.end_turn_after_responses();
        self.console.print("  denied; the turn ends once the denial is recorded\n");
        PromptAnswer::Deny { message }
    }

    async fn remember(&self, suggestions: &[RuleSuggestion]) -> Remembered {
        let mut text = String::from("  remember which rule?\n");
        for (index, suggestion) in suggestions.iter().enumerate() {
            let scope = match suggestion.scope {
                adk_harness::SuggestedScope::User => "every session",
                adk_harness::SuggestedScope::Session => "this session",
            };
            text.push_str(&format!(
                "    [{}] {} ({scope}){}\n",
                index + 1,
                terminal_safe(&suggestion.label),
                if suggestion.runs_scripts {
                    " — runs project scripts, so it allows whatever they do"
                } else {
                    ""
                }
            ));
        }
        self.console.print(&text);
        let prompt = format!("  rule [1-{}], or Enter to go back > ", suggestions.len());
        match self.console.read_line(&prompt).await {
            ConsoleInput::Line(line) => match line.trim().parse::<usize>() {
                Ok(choice) if (1..=suggestions.len()).contains(&choice) => {
                    Remembered::Rule(suggestions[choice - 1].clone())
                }
                _ => Remembered::Back,
            },
            ConsoleInput::Interrupted => Remembered::Interrupted,
            ConsoleInput::Eof => Remembered::Back,
        }
    }
}

enum Remembered {
    Rule(RuleSuggestion),
    Back,
    Interrupted,
}

fn kind_label(kind: &ToolKind) -> String {
    match kind {
        ToolKind::Web { allowlisted: true } => "web fetch of an allowlisted host".to_string(),
        ToolKind::Web { allowlisted: false } => {
            "web fetch of a host outside the allowlist".to_string()
        }
        ToolKind::Read => "read".into(),
        ToolKind::Todo => "todo".into(),
        ToolKind::PlanExit => "plan exit".into(),
        ToolKind::Edit => "edit".into(),
        ToolKind::Shell(_) => "shell".into(),
        ToolKind::ShellEscalation => "unsandboxed retry".into(),
        ToolKind::Delegate => "delegation".into(),
        ToolKind::MemoryWrite => "memory write".into(),
        ToolKind::MemoryPropose => "fact proposal".into(),
        ToolKind::Payment => "payment".into(),
        ToolKind::External { trusted_origin, .. } => {
            if *trusted_origin {
                "external tool".into()
            } else {
                "external tool (untrusted)".into()
            }
        }
        // `ToolKind` is non-exhaustive; a class added later is still shown.
        other => format!("{other:?}").to_lowercase(),
    }
}

#[async_trait]
impl ApprovalPrompter for ConsolePrompter {
    async fn prompt(&self, prompt: ApprovalPrompt) -> Result<PromptAnswer> {
        if let Some(allow_unsandboxed) = self.approve_all {
            return Ok(self.approved_unattended(&prompt, allow_unsandboxed));
        }
        if self.console.run_interrupted() {
            return Ok(PromptAnswer::DenyAllRemaining);
        }
        if !self.console.is_terminal() {
            return Ok(self.deny_unattended(&prompt));
        }

        self.show(&prompt);
        let original = prompt.request.args.clone();
        let mut args: Value = original.clone();
        let menu = if prompt.suggestions.is_empty() { MENU_ONCE } else { MENU };
        loop {
            let answer = match self.console.read_line(menu).await {
                ConsoleInput::Line(answer) => answer,
                ConsoleInput::Interrupted => return Ok(self.interrupted()),
                ConsoleInput::Eof => {
                    return Ok(PromptAnswer::Deny {
                        message: Some("input ended at the prompt".into()),
                    });
                }
            };
            match answer.trim() {
                "y" | "yes" => {
                    if !prompt.rendered.can_approve() {
                        self.console.print("  press ? to see the whole call before approving it\n");
                        continue;
                    }
                    return Ok(if args == original {
                        PromptAnswer::ApproveOnce
                    } else {
                        PromptAnswer::ApproveWithArgs(args)
                    });
                }
                "a" | "always" if !prompt.suggestions.is_empty() => {
                    if !prompt.rendered.can_approve() {
                        self.console.print("  press ? to see the whole call before approving it\n");
                        continue;
                    }
                    if args != original {
                        self.console.print(
                            "  a rule is remembered for the call as the model made it; approve \
                             the edited call once with y\n",
                        );
                        continue;
                    }
                    match self.remember(&prompt.suggestions).await {
                        Remembered::Rule(rule) => {
                            return Ok(PromptAnswer::ApproveAndRemember(rule));
                        }
                        Remembered::Back => {}
                        Remembered::Interrupted => return Ok(self.interrupted()),
                    }
                }
                "a" | "always" => self.console.print(
                    "  no standing rule can cover this call (a non-waivable call, or a command \
                     that is not plain words); [y] allows it once\n",
                ),
                "n" | "no" => {
                    return Ok(match self.console.read_line("  reason (optional) > ").await {
                        ConsoleInput::Interrupted => self.interrupted(),
                        ConsoleInput::Line(reason) if !reason.trim().is_empty() => {
                            self.denied(Some(reason.trim().to_string()))
                        }
                        ConsoleInput::Line(_) | ConsoleInput::Eof => self.denied(None),
                    });
                }
                "e" | "edit" => match edit_json(&self.console, &args).await {
                    crate::console::Edited::Args(edited) => {
                        args = edited;
                        self.console.print(&format!(
                            "  edited: {} {}\n",
                            terminal_safe(&prompt.request.tool_name),
                            terminal_safe(&args.to_string()),
                        ));
                    }
                    crate::console::Edited::Unchanged(note) => {
                        self.console.print(&format!("  {note}\n"));
                    }
                    crate::console::Edited::Interrupted => return Ok(self.interrupted()),
                },
                "?" | "details" => {
                    let mut text = String::from("  full call:\n");
                    for line in prompt.rendered.expand().lines() {
                        text.push_str(&format!("    {}\n", terminal_safe(line)));
                    }
                    if args != original {
                        text.push_str(&format!(
                            "  edited arguments: {}\n",
                            terminal_safe(&args.to_string())
                        ));
                    }
                    if let Some(id) = &prompt.request.function_call_id {
                        text.push_str(&format!("  call id: {}\n", terminal_safe(id)));
                    }
                    self.console.print(&text);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_core::{ToolConfirmationRequest, ToolOrigin};
    use adk_harness::{HarnessMode, IsolationStatus, RenderedCall, StandingRule};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    fn line(text: &str) -> ConsoleInput {
        ConsoleInput::Line(text.into())
    }

    fn bash(command: &str) -> ApprovalPrompt {
        let request = ToolConfirmationRequest::new("bash", json!({ "command": command }))
            .with_function_call_id("call-1")
            .with_origin(ToolOrigin::new("static", &json!({ "name": "bash" }), None));
        let suggestions = StandingRule::suggestions(&request);
        let rendered = RenderedCall::render(&request, &IsolationStatus::Host, 4096);
        ApprovalPrompt {
            request,
            kind: ToolKind::Shell(adk_harness::Isolation::Host),
            mode: HarnessMode::Ask,
            isolation: IsolationStatus::Host,
            suggestions,
            rendered,
        }
    }

    fn scripted(inputs: impl IntoIterator<Item = ConsoleInput>) -> (ConsolePrompter, ConsoleIo) {
        let console = ConsoleIo::scripted(inputs, true);
        (ConsolePrompter::new(console.clone()), console)
    }

    #[tokio::test]
    async fn y_approves_once() {
        let (prompter, console) = scripted([line("y")]);
        assert_eq!(prompter.prompt(bash("cargo test")).await.unwrap(), PromptAnswer::ApproveOnce);
        let shown = console.transcript();
        assert!(shown.contains("[approval needed] bash on host"), "{shown}");
        assert!(shown.contains("$ cargo test"), "{shown}");
        assert!(shown.contains(MENU), "{shown}");
    }

    #[tokio::test]
    async fn a_remembers_the_chosen_suggestion() {
        let prompt = bash("cargo test --quiet");
        let first = prompt.suggestions[0].clone();
        let (prompter, console) = scripted([line("a"), line("1")]);
        assert_eq!(prompter.prompt(prompt).await.unwrap(), PromptAnswer::ApproveAndRemember(first));
        let shown = console.transcript();
        assert!(shown.contains("[1] bash: cargo test … (every session)"), "{shown}");
        assert!(shown.contains("runs project scripts"), "{shown}");
    }

    #[tokio::test]
    async fn a_is_not_offered_without_suggestions() {
        // A chained command gets no rule suggestion, so `a` does nothing.
        let (prompter, console) = scripted([line("a"), line("y")]);
        assert_eq!(prompter.prompt(bash("ls; rm -rf ~")).await.unwrap(), PromptAnswer::ApproveOnce);
        assert!(console.transcript().contains(MENU_ONCE));
        assert!(console.transcript().contains("no standing rule can cover this call"));
        assert!(!console.transcript().contains("remember which rule"));
    }

    #[tokio::test]
    async fn n_denies_with_a_reason_and_ends_the_turn() {
        let (prompter, console) = scripted([line("n"), line("  use the staging db  ")]);
        console.set_run(Some(CancellationToken::new()));
        assert_eq!(
            prompter.prompt(bash("psql prod")).await.unwrap(),
            PromptAnswer::Deny { message: Some("use the staging db".into()) }
        );
        assert!(console.end_turn_requested());
        assert!(console.transcript().contains("the turn ends"));
    }

    #[tokio::test]
    async fn e_edits_the_arguments_and_y_approves_the_edit() {
        let (prompter, _) =
            scripted([line("e"), line(r#"{"command": "cargo test -p a"}"#), line("y")]);
        assert_eq!(
            prompter.prompt(bash("cargo test")).await.unwrap(),
            PromptAnswer::ApproveWithArgs(json!({ "command": "cargo test -p a" }))
        );
    }

    #[tokio::test]
    async fn an_edited_call_is_not_remembered() {
        let (prompter, console) =
            scripted([line("e"), line(r#"{"command": "cargo test -p a"}"#), line("a"), line("y")]);
        assert!(matches!(
            prompter.prompt(bash("cargo test")).await.unwrap(),
            PromptAnswer::ApproveWithArgs(_)
        ));
        assert!(console.transcript().contains("approve the edited call once"));
    }

    #[tokio::test]
    async fn ctrl_c_denies_the_rest_of_the_turn_and_cancels_the_run() {
        let (prompter, console) = scripted([ConsoleInput::Interrupted]);
        let run = CancellationToken::new();
        console.set_run(Some(run.clone()));
        assert_eq!(prompter.prompt(bash("make")).await.unwrap(), PromptAnswer::DenyAllRemaining);
        assert!(run.is_cancelled());
        // A queued call is denied without a prompt.
        let shown = console.transcript();
        assert_eq!(prompter.prompt(bash("make")).await.unwrap(), PromptAnswer::DenyAllRemaining);
        assert_eq!(console.transcript(), shown);
    }

    #[tokio::test]
    async fn without_a_terminal_the_call_is_denied_with_guidance() {
        let console = ConsoleIo::scripted([line("y")], false);
        let prompter = ConsolePrompter::new(console.clone());
        assert!(matches!(prompter.prompt(bash("make")).await.unwrap(), PromptAnswer::Deny { .. }));
        // The denial is shown once the console has shown the call it belongs to.
        assert!(!console.transcript().contains("pass --yes"));
        console.call_shown("call-1");
        assert!(console.transcript().contains("pass --yes"));
        assert_eq!(console.read_line("").await, line("y"));
    }

    #[tokio::test]
    async fn an_unattended_decision_follows_the_calls_line() {
        let console = ConsoleIo::scripted([], false);
        let prompter = ConsolePrompter::new(console.clone()).approve_all(false);
        console.call_shown("call-0");
        assert_eq!(prompter.prompt(bash("cargo test")).await.unwrap(), PromptAnswer::ApproveOnce);
        assert!(!console.transcript().contains("[approved by --yes]"), "{}", console.transcript());
        console.print("[tool-call] bash\n");
        console.call_shown("call-1");
        let shown = console.transcript();
        let call = shown.find("[tool-call] bash").expect(&shown);
        let approved = shown.find("[approved by --yes] bash on host").expect(&shown);
        assert!(call < approved, "{shown}");
        // A prompt for a call already shown prints at once.
        assert_eq!(prompter.prompt(bash("cargo test")).await.unwrap(), PromptAnswer::ApproveOnce);
        assert_eq!(
            shown.matches("[approved by --yes]").count() + 1,
            console.transcript().matches("[approved by --yes]").count()
        );
    }

    #[tokio::test]
    async fn a_truncated_call_needs_details_before_approval() {
        let mut prompt = bash(&"x".repeat(10_000));
        prompt.rendered = RenderedCall::render(&prompt.request, &IsolationStatus::Host, 100);
        let (prompter, console) = scripted([line("y"), line("?"), line("y")]);
        assert_eq!(prompter.prompt(prompt).await.unwrap(), PromptAnswer::ApproveOnce);
        let shown = console.transcript();
        assert!(shown.contains("press ? to see the whole call"), "{shown}");
        assert!(shown.contains(&"x".repeat(10_000)), "the details show the whole call");
    }

    #[tokio::test]
    async fn yes_approves_but_never_an_unsandboxed_retry_without_the_flag() {
        let console = ConsoleIo::scripted([], false);
        let prompter = ConsolePrompter::new(console.clone()).approve_all(false);
        assert_eq!(prompter.prompt(bash("cargo test")).await.unwrap(), PromptAnswer::ApproveOnce);
        let mut retry = bash("cargo test");
        retry.kind = ToolKind::ShellEscalation;
        assert!(matches!(prompter.prompt(retry.clone()).await.unwrap(), PromptAnswer::Deny { .. }));
        let allowing = ConsolePrompter::new(console.clone()).approve_all(true);
        assert_eq!(allowing.prompt(retry).await.unwrap(), PromptAnswer::ApproveOnce);
        console.flush_call_prints();
        assert!(console.transcript().contains("[approved by --yes] bash on host"));
    }

    #[tokio::test]
    async fn planted_escape_sequences_are_shown_escaped() {
        let (prompter, console) = scripted([line("?"), line("n"), line("")]);
        prompter.prompt(bash("echo \x1b]52;c;x\x07 \x1b[2J")).await.unwrap();
        assert!(!console.transcript().contains('\x1b'), "{:?}", console.transcript());
    }
}
