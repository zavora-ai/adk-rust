//! Change sets (spec 3.1 §7.26, §9.2 I66): the held edits of one model turn reviewed as
//! one diff and decided once, each approved call bound to the content it was reviewed
//! against.

#![cfg(feature = "runner")]

use crate::common;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use adk_harness::{
    ApprovalPrompt, ApprovalPrompter, ChangeSetAnswer, ChangeSetPrompt, HarnessMode, PromptAnswer,
    RulePredicate,
};
use adk_model::ScriptedResponse;
use common::{Outcome, fixture, outcome, responses};
use serde_json::{Value, json};

type CardAnswer = Box<dyn Fn(&ChangeSetPrompt) -> ChangeSetAnswer + Send + Sync>;
type SingleAnswer = Box<dyn Fn(&ApprovalPrompt) -> PromptAnswer + Send + Sync>;

/// Answers change sets and single prompts from queues, and records both.
#[derive(Default)]
struct CardPrompter {
    cards: Mutex<VecDeque<CardAnswer>>,
    singles: Mutex<VecDeque<SingleAnswer>>,
    shown: Mutex<Vec<ChangeSetPrompt>>,
    prompted: Mutex<Vec<ApprovalPrompt>>,
    /// Shows calls one at a time, as a prompter without a card does.
    no_cards: bool,
}

impl std::fmt::Debug for CardPrompter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CardPrompter")
    }
}

impl CardPrompter {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn card(&self, answer: impl Fn(&ChangeSetPrompt) -> ChangeSetAnswer + Send + Sync + 'static) {
        self.cards.lock().unwrap().push_back(Box::new(answer));
    }

    fn single(&self, answer: PromptAnswer) {
        self.singles.lock().unwrap().push_back(Box::new(move |_| answer.clone()));
    }

    fn single_with(
        &self,
        answer: impl Fn(&ApprovalPrompt) -> PromptAnswer + Send + Sync + 'static,
    ) {
        self.singles.lock().unwrap().push_back(Box::new(answer));
    }

    fn cards_shown(&self) -> Vec<ChangeSetPrompt> {
        self.shown.lock().unwrap().clone()
    }

    fn prompts(&self) -> usize {
        self.prompted.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl ApprovalPrompter for CardPrompter {
    async fn prompt(&self, prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        let answer = self.singles.lock().unwrap().pop_front();
        let answer = match &answer {
            Some(answer) => answer(&prompt),
            None => PromptAnswer::Deny { message: Some("no scripted answer".into()) },
        };
        self.prompted.lock().unwrap().push(prompt);
        Ok(answer)
    }

    async fn prompt_change_set(
        &self,
        prompt: ChangeSetPrompt,
    ) -> adk_core::Result<Option<ChangeSetAnswer>> {
        if self.no_cards {
            return Ok(None);
        }
        let answer = self.cards.lock().unwrap().pop_front();
        let answer = match &answer {
            Some(answer) => answer(&prompt),
            None => ChangeSetAnswer::DenyAll { message: Some("no scripted answer".into()) },
        };
        self.shown.lock().unwrap().push(prompt);
        Ok(Some(answer))
    }
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

fn write(path: &str, content: &str) -> (&'static str, Value) {
    ("write_file", json!({ "path": path, "content": content }))
}

fn edit(path: &str, old: &str, new: &str) -> (&'static str, Value) {
    ("edit_file", json!({ "path": path, "old_string": old, "new_string": new }))
}

fn read(dir: &std::path::Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap_or_default()
}

fn outcomes(events: &[adk_core::Event]) -> Vec<Outcome> {
    responses(events).iter().map(|(_, response)| outcome(response)).collect()
}

/// A workspace with `src/lib.rs`, read by the agent so an edit of it may run.
fn with_lib(f: &common::Fixture) {
    std::fs::create_dir_all(f.path("src")).unwrap();
    std::fs::write(f.path("src/lib.rs"), "pub fn one() -> u32 {\n    1\n}\n").unwrap();
}

#[tokio::test]
async fn changeset_one_approval() {
    // spec 3.1 §11 `changeset_one_approval`: two held edits, one card, one answer.
    let prompter = CardPrompter::new();
    prompter.card(|_| ChangeSetAnswer::ApproveAll);
    let batch = ScriptedResponse::function_calls([
        edit("src/lib.rs", "    1", "    2"),
        write("docs/notes.md", "# Notes\n"),
    ]);
    let f = fixture(vec![batch, done()], |b| b.prompter(prompter.clone())).await;
    with_lib(&f);

    let events = f.send("change both").await;

    assert_eq!(
        outcomes(&events),
        [Outcome::Executed, Outcome::Executed],
        "{:#?}",
        responses(&events)
    );
    assert_eq!(prompter.prompts(), 0, "no call was asked about on its own");
    let cards = prompter.cards_shown();
    assert_eq!(cards.len(), 1);
    let card = &cards[0];
    let paths: Vec<&str> = card.change_set.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["src/lib.rs", "docs/notes.md"]);
    assert_eq!(card.change_set.files[1].before_sha256, None, "a new file");
    assert!(
        card.change_set.files[0].diff.contains("-    1\n+    2"),
        "{}",
        card.change_set.files[0].diff
    );
    assert!(card.change_set.digest.starts_with("sha256:"));
    assert_eq!(read(f.dir.path(), "src/lib.rs"), "pub fn one() -> u32 {\n    2\n}\n");
    assert_eq!(read(f.dir.path(), "docs/notes.md"), "# Notes\n");

    let decided = f.records("harness.changeset.decided");
    assert_eq!(decided.len(), 1);
    assert_eq!(decided[0].ext["digest"], json!(card.change_set.digest));
    assert_eq!(decided[0].ext["answer"], "approve_all");
    assert_eq!(decided[0].ext["callIds"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn successive_edits_of_one_file_compose_and_each_is_bound_to_its_point() {
    let prompter = CardPrompter::new();
    prompter.card(|_| ChangeSetAnswer::ApproveAll);
    let batch = ScriptedResponse::function_calls([
        edit("src/lib.rs", "one", "first"),
        edit("src/lib.rs", "first", "second"),
    ]);
    let f = fixture(vec![batch, done()], |b| b.prompter(prompter.clone())).await;
    with_lib(&f);

    let events = f.send("rename twice").await;

    assert_eq!(
        outcomes(&events),
        [Outcome::Executed, Outcome::Executed],
        "{:#?}",
        responses(&events)
    );
    let card = &prompter.cards_shown()[0];
    assert_eq!(card.change_set.files.len(), 1, "one file, both edits");
    assert!(card.change_set.files[0].diff.contains("+pub fn second()"));
    let first = card.change_set.calls[0].approved_args().unwrap();
    let second = card.change_set.calls[1].approved_args().unwrap();
    assert_ne!(
        first["expect_sha256"], second["expect_sha256"],
        "the second binds the intermediate"
    );
    assert_eq!(read(f.dir.path(), "src/lib.rs"), "pub fn second() -> u32 {\n    1\n}\n");
}

#[tokio::test]
async fn a_call_that_would_fail_is_shown_failing_and_answered_without_running() {
    let prompter = CardPrompter::new();
    prompter.card(|_| ChangeSetAnswer::ApproveAll);
    let batch = ScriptedResponse::function_calls([
        write("a.md", "a\n"),
        edit("src/lib.rs", "not in the file", "x"),
        write("b.md", "b\n"),
    ]);
    let f = fixture(vec![batch, done()], |b| b.prompter(prompter.clone())).await;
    with_lib(&f);

    let events = f.send("go").await;

    assert_eq!(
        outcomes(&events),
        [Outcome::Executed, Outcome::Denied, Outcome::Executed],
        "{:#?}",
        responses(&events)
    );
    let failing = &prompter.cards_shown()[0].change_set.calls[1];
    assert!(
        failing.failure.as_deref().unwrap_or_default().contains("no occurrence"),
        "{failing:?}"
    );
    let (_, response) = &responses(&events)[1];
    assert!(response.to_string().contains("no occurrence"), "the model sees why: {response}");
    assert_eq!(read(f.dir.path(), "src/lib.rs"), "pub fn one() -> u32 {\n    1\n}\n");
}

#[tokio::test]
async fn a_protected_edit_is_kept_out_of_the_card_and_asked_about_after_it() {
    let prompter = CardPrompter::new();
    prompter.card(|_| ChangeSetAnswer::ApproveAll);
    prompter.single(PromptAnswer::Deny { message: Some("leave AGENTS.md alone".into()) });
    let batch = ScriptedResponse::function_calls([
        write("AGENTS.md", "pwned\n"),
        write("a.md", "a\n"),
        write("b.md", "b\n"),
    ]);
    let f = fixture(vec![batch, done()], |b| b.prompter(prompter.clone())).await;

    let events = f.send("go").await;

    let card = &prompter.cards_shown()[0];
    assert_eq!(card.change_set.files.len(), 2, "the protected file is not merged in");
    assert_eq!(card.asked_separately, 1);
    assert_eq!(prompter.prompts(), 1, "AGENTS.md is prompted on its own");
    assert_eq!(
        outcomes(&events),
        [Outcome::Denied, Outcome::Executed, Outcome::Executed],
        "{:#?}",
        responses(&events)
    );
    assert!(!f.path("AGENTS.md").exists());
}

#[tokio::test]
async fn approving_selected_files_runs_only_their_calls() {
    let prompter = CardPrompter::new();
    prompter.card(|_| ChangeSetAnswer::ApproveFiles(vec!["a.md".into()]));
    let batch = ScriptedResponse::function_calls([write("a.md", "a\n"), write("b.md", "b\n")]);
    let f = fixture(vec![batch, done()], |b| b.prompter(prompter.clone())).await;

    let events = f.send("go").await;

    assert_eq!(outcomes(&events), [Outcome::Executed, Outcome::Denied]);
    assert!(f.path("a.md").exists() && !f.path("b.md").exists());
}

#[tokio::test]
async fn denying_the_change_set_runs_nothing_and_the_model_sees_the_reason() {
    let prompter = CardPrompter::new();
    prompter.card(|_| ChangeSetAnswer::DenyAll { message: Some("use the staging docs".into()) });
    let batch = ScriptedResponse::function_calls([write("a.md", "a\n"), write("b.md", "b\n")]);
    let f = fixture(vec![batch, done()], |b| b.prompter(prompter.clone())).await;

    let events = f.send("go").await;

    assert_eq!(outcomes(&events), [Outcome::Denied, Outcome::Denied]);
    assert!(responses(&events)[0].1.to_string().contains("use the staging docs"));
    assert!(!f.path("a.md").exists() && !f.path("b.md").exists());
}

#[tokio::test]
async fn a_file_changed_after_the_card_was_shown_is_not_written() {
    // I66: an approved edit runs only against the content the person reviewed.
    let prompter = CardPrompter::new();
    let lib = Arc::new(Mutex::new(None::<std::path::PathBuf>));
    let seen = lib.clone();
    prompter.card(move |_| {
        if let Some(path) = seen.lock().unwrap().as_ref() {
            std::fs::write(path, "pub fn one() -> u32 {\n    1\n}\n// changed meanwhile\n")
                .unwrap();
        }
        ChangeSetAnswer::ApproveAll
    });
    let batch = ScriptedResponse::function_calls([
        edit("src/lib.rs", "    1", "    2"),
        write("a.md", "a\n"),
    ]);
    let f = fixture(vec![batch, done()], |b| b.prompter(prompter.clone())).await;
    with_lib(&f);
    *lib.lock().unwrap() = Some(f.path("src/lib.rs"));

    let events = f.send("go").await;

    let (_, response) = &responses(&events)[0];
    assert!(response.to_string().contains("devtools.stale"), "{response}");
    assert!(read(f.dir.path(), "src/lib.rs").contains("changed meanwhile"));
    assert!(read(f.dir.path(), "src/lib.rs").contains("    1\n"), "the edit did not run");
    assert_eq!(
        outcome(&responses(&events)[1].1),
        Outcome::Executed,
        "a stale call refuses only itself"
    );
}

#[tokio::test]
async fn an_edited_change_set_is_shown_again_and_runs_with_the_edited_arguments() {
    let prompter = CardPrompter::new();
    prompter.card(|card| {
        let id = card.change_set.calls[1].function_call_id.clone();
        ChangeSetAnswer::Edit(vec![(id, json!({ "path": "b.md", "content": "edited\n" }))])
    });
    prompter.card(|card| {
        assert!(
            card.change_set.files[1].diff.contains("+edited"),
            "{}",
            card.change_set.files[1].diff
        );
        ChangeSetAnswer::ApproveAll
    });
    let batch = ScriptedResponse::function_calls([write("a.md", "a\n"), write("b.md", "b\n")]);
    let f = fixture(vec![batch, done()], |b| b.prompter(prompter.clone())).await;

    let events = f.send("go").await;

    assert_eq!(outcomes(&events), [Outcome::Executed, Outcome::Executed]);
    assert_eq!(prompter.cards_shown().len(), 2);
    assert_eq!(read(f.dir.path(), "b.md"), "edited\n");
    assert_eq!(f.records("harness.changeset.decided")[0].ext["edited"], true);
}

#[tokio::test]
async fn edits_a_standing_rule_allows_are_counted_not_held() {
    let prompter = CardPrompter::new();
    // The first turn's prompt remembers "write_file within src/", as `[a]` does.
    prompter.single_with(|prompt| {
        let within_src = prompt
            .suggestions
            .iter()
            .find(|suggestion| {
                suggestion.predicates.iter().any(|predicate| {
                    matches!(predicate, RulePredicate::PathWithin { dir, .. } if dir == "src")
                })
            })
            .expect("a write in src offers a rule for src")
            .clone();
        PromptAnswer::ApproveAndRemember(within_src)
    });
    prompter.card(|_| ChangeSetAnswer::ApproveAll);
    let first = ScriptedResponse::function_calls([write("src/first.rs", "x\n")]);
    let batch = ScriptedResponse::function_calls([
        write("src/rule.rs", "r\n"),
        write("docs/a.md", "a\n"),
        write("docs/b.md", "b\n"),
    ]);
    let f = fixture(vec![first, done(), batch, done()], |b| b.prompter(prompter.clone())).await;
    f.send("first").await;
    assert_eq!(prompter.prompts(), 1);

    let events = f.send("batch").await;

    assert_eq!(outcomes(&events), [Outcome::Executed, Outcome::Executed, Outcome::Executed]);
    let card = &prompter.cards_shown()[0];
    let paths: Vec<&str> = card.change_set.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["docs/a.md", "docs/b.md"]);
    assert_eq!(card.under_rules, 1, "the rule's edit is counted, not held");
    assert_eq!(prompter.prompts(), 1, "nothing else was asked");
    assert_eq!(card.mode, HarnessMode::Ask);
}

#[tokio::test]
async fn a_prompter_without_cards_asks_about_each_call() {
    let prompter = Arc::new(CardPrompter { no_cards: true, ..CardPrompter::default() });
    prompter.single(PromptAnswer::ApproveOnce);
    prompter.single(PromptAnswer::ApproveOnce);
    let batch = ScriptedResponse::function_calls([write("a.md", "a\n"), write("b.md", "b\n")]);
    let f = fixture(vec![batch, done()], |b| b.prompter(prompter.clone())).await;

    let events = f.send("go").await;

    assert_eq!(outcomes(&events), [Outcome::Executed, Outcome::Executed]);
    assert_eq!(prompter.prompts(), 2);
    assert!(f.records("harness.changeset.decided").is_empty());
}
