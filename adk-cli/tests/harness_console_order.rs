//! The console shows a decision when it is taken, before the call's output (spec 3.1 §7.33
//! fix 2a). The model is scripted and the console is a scripted terminal, so the transcript
//! holds the prompt, the decision and the streamed output in the order they were printed.

#![cfg(all(feature = "harness", unix))]

use std::sync::Arc;

use adk_cli::console::{ConsoleInput, ConsoleIo};
use adk_cli::harness::{ConsolePrompter, HarnessLauncher};
use adk_harness::{HarnessAgent, HarnessMode, InstructionsConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_session::InMemorySessionService;
use serde_json::json;

#[tokio::test]
async fn a_held_bash_prints_its_decision_before_its_first_output_line() {
    let dir = tempfile::tempdir().unwrap();
    let console = ConsoleIo::scripted([ConsoleInput::Line("y".into())], true);
    let model = ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "bash",
                json!({ "command": "echo first-line; sleep 0.2; echo second-line" }),
            ),
            ScriptedResponse::text("Done."),
        ],
    );
    let agent = HarnessAgent::builder("adk-rust")
        .model(Arc::new(model))
        .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
        .project_instructions(InstructionsConfig {
            user_file: None,
            ..InstructionsConfig::default()
        })
        .mode(HarnessMode::Ask)
        .without_default_members()
        .prompter(Arc::new(ConsolePrompter::new(console.clone())))
        .build()
        .unwrap();
    let mut launcher = HarnessLauncher::new(agent)
        .with_session_service(Arc::new(InMemorySessionService::new()))
        .with_console(console.clone())
        .trust_workspace(true);

    let outcome = launcher.run_once("say hello twice").await.unwrap();
    assert!(outcome.held.is_empty(), "{outcome:?}");

    let transcript = console.transcript();
    let position = |needle: &str| {
        transcript.find(needle).unwrap_or_else(|| panic!("no {needle:?} in:\n{transcript}"))
    };
    let prompt = position("[y] allow once");
    let approved = position("[approved] bash");
    let header = position("[bash ");
    // On a line of its own: the call's line quotes the command too.
    let output = position("\nfirst-line\n");
    assert!(prompt < approved && approved < header && header < output, "{transcript}");
    assert_eq!(transcript.matches("[approved] bash").count(), 1, "{transcript}");
    assert!(position("\nsecond-line\n") < position("[tool-response] bash"), "{transcript}");
}
