//! The change-set card at the console (spec 3.1 §7.26): two or more held edits of one
//! model turn shown as one diff and decided with one answer.

use adk_harness::{ChangeSetAnswer, ChangeSetPrompt};

use super::prompter::ConsolePrompter;
use crate::console::{ConsoleInput, edit_text};
use crate::launcher::terminal_safe;

const MENU: &str =
    "  [y] approve all  [f] choose files  [n] deny all (reason)  [e] edit  [?] full diff > ";

impl ConsolePrompter {
    /// Shows `card` and reads the person's one answer; prints the decision for each call.
    pub(super) async fn change_set(&self, card: ChangeSetPrompt) -> Option<ChangeSetAnswer> {
        let title = terminal_safe(&card.title()).into_owned();
        if let Some(_allow_unsandboxed) = self.approve_all_flag() {
            let mut text = format!("\n[approved by --yes] {title}\n");
            for line in card.rendered.expand().lines() {
                text.push_str(&format!("  {}\n", terminal_safe(line)));
            }
            self.console().print(&text);
            self.print_card_decisions(&card, &ChangeSetAnswer::ApproveAll);
            return Some(ChangeSetAnswer::ApproveAll);
        }
        if self.console().run_interrupted() {
            return Some(ChangeSetAnswer::DenyAll { message: None });
        }
        if !self.console().is_terminal() {
            self.console().print(&format!(
                "\n[approval needed] {title} — denied: there is no terminal to ask. Run \
                 interactively, pass --yes to approve calls, or --mode plan to plan without \
                 changes.\n"
            ));
            return Some(ChangeSetAnswer::DenyAll {
                message: Some("no terminal was available to ask for approval".into()),
            });
        }

        self.show_card(&card);
        loop {
            let answer = match self.console().read_line(MENU).await {
                ConsoleInput::Line(answer) => answer,
                ConsoleInput::Interrupted => {
                    self.console().interrupt_run();
                    self.console().print("  denied; the run is interrupted\n");
                    return Some(ChangeSetAnswer::DenyAll { message: None });
                }
                ConsoleInput::Eof => {
                    return Some(ChangeSetAnswer::DenyAll {
                        message: Some("input ended at the prompt".into()),
                    });
                }
            };
            match answer.trim() {
                "y" | "yes" => {
                    if !card.rendered.can_approve() {
                        self.console()
                            .print("  press ? to see the whole diff before approving it\n");
                        continue;
                    }
                    self.print_card_decisions(&card, &ChangeSetAnswer::ApproveAll);
                    return Some(ChangeSetAnswer::ApproveAll);
                }
                "f" | "files" => {
                    if !card.rendered.can_approve() {
                        self.console()
                            .print("  press ? to see the whole diff before approving it\n");
                        continue;
                    }
                    let files = &card.change_set.files;
                    let mut text = String::from("  approve which files?\n");
                    for (index, file) in files.iter().enumerate() {
                        text.push_str(&format!(
                            "    [{}] {}\n",
                            index + 1,
                            terminal_safe(&file.path)
                        ));
                    }
                    self.console().print(&text);
                    let chosen = match self
                        .console()
                        .read_line("  files (e.g. 1,3), or Enter to go back > ")
                        .await
                    {
                        ConsoleInput::Line(line) => line,
                        ConsoleInput::Interrupted => {
                            self.console().interrupt_run();
                            return Some(ChangeSetAnswer::DenyAll { message: None });
                        }
                        ConsoleInput::Eof => continue,
                    };
                    let selected: Vec<String> = chosen
                        .split([',', ' '])
                        .filter_map(|part| part.trim().parse::<usize>().ok())
                        .filter_map(|number| files.get(number.wrapping_sub(1)))
                        .map(|file| file.path.clone())
                        .collect();
                    if selected.is_empty() {
                        continue;
                    }
                    let answer = ChangeSetAnswer::ApproveFiles(selected);
                    self.print_card_decisions(&card, &answer);
                    return Some(answer);
                }
                "n" | "no" => {
                    let message = match self.console().read_line("  reason (optional) > ").await {
                        ConsoleInput::Interrupted => {
                            self.console().interrupt_run();
                            return Some(ChangeSetAnswer::DenyAll { message: None });
                        }
                        ConsoleInput::Line(reason) if !reason.trim().is_empty() => {
                            Some(reason.trim().to_string())
                        }
                        ConsoleInput::Line(_) | ConsoleInput::Eof => None,
                    };
                    self.console().end_turn_after_responses();
                    let answer = ChangeSetAnswer::DenyAll { message };
                    self.print_card_decisions(&card, &answer);
                    return Some(answer);
                }
                "e" | "edit" => {
                    match edit_text(self.console(), &card.edit_document(), "json").await {
                        Ok(Some(text)) => match ChangeSetAnswer::from_edit_document(&text) {
                            Ok(answer) => {
                                self.console().print("  edited; the change set is shown again\n");
                                return Some(answer);
                            }
                            Err(error) => {
                                self.console().print(&format!("  {error}; nothing changed\n"))
                            }
                        },
                        Ok(None) => self.console().print("  nothing changed\n"),
                        Err(()) => {
                            self.console().interrupt_run();
                            return Some(ChangeSetAnswer::DenyAll { message: None });
                        }
                    }
                }
                "?" | "details" => {
                    let mut text = String::from("  full diff:\n");
                    for line in card.rendered.expand().lines() {
                        text.push_str(&format!("    {}\n", terminal_safe(line)));
                    }
                    self.console().print(&text);
                }
                _ => {}
            }
        }
    }

    fn show_card(&self, card: &ChangeSetPrompt) {
        let mut text = format!("\n[approval needed] {}\n", terminal_safe(&card.title()));
        for line in card.rendered.body.lines() {
            text.push_str(&format!("  {}\n", terminal_safe(line)));
        }
        text.push_str(&format!(
            "  edits · mode {} · agent {}\n",
            card.mode,
            terminal_safe(&card.agent)
        ));
        if card.under_rules > 0 {
            text.push_str(&format!(
                "  {} more edit{} run{} under standing rules\n",
                card.under_rules,
                if card.under_rules == 1 { "" } else { "s" },
                if card.under_rules == 1 { "s" } else { "" },
            ));
        }
        if card.asked_separately > 0 {
            text.push_str(&format!(
                "  {} edit{} of a protected or shared file {} asked about on {} own after this\n",
                card.asked_separately,
                if card.asked_separately == 1 { "" } else { "s" },
                if card.asked_separately == 1 { "is" } else { "are" },
                if card.asked_separately == 1 { "its" } else { "their" },
            ));
        }
        if card.rendered.truncated {
            text.push_str("  (diff cut for display; press ? to see it before approving)\n");
        }
        self.console().print(&text);
    }

    /// `[approved] edit_file src/lib.rs` or `[denied] …` for each call, printed after the
    /// call's line as single decisions are (spec 3.1 §7.33 fix 2a).
    fn print_card_decisions(&self, card: &ChangeSetPrompt, answer: &ChangeSetAnswer) {
        let approved: std::collections::HashSet<&str> = match answer {
            ChangeSetAnswer::ApproveAll => {
                card.change_set.files.iter().map(|file| file.path.as_str()).collect()
            }
            ChangeSetAnswer::ApproveFiles(files) => files.iter().map(String::as_str).collect(),
            ChangeSetAnswer::DenyAll { .. } | ChangeSetAnswer::Edit(_) => Default::default(),
        };
        for call in &card.change_set.calls {
            let files = call.files.join(", ");
            let label = match &call.failure {
                Some(failure) => format!("[not run] {} — {failure}", call.tool),
                None if call.files.iter().all(|file| approved.contains(file.as_str())) => {
                    format!("[approved] {} {files}", call.tool)
                }
                None => format!("[denied] {} {files}", call.tool),
            };
            self.console().print_decision(
                Some(call.function_call_id.as_str()),
                &format!("{}\n", terminal_safe(&label)),
            );
        }
    }
}
