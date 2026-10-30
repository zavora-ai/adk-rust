//! The personal preset (spec 3.3 §10 H1): a workspace that is not a code repository, no host
//! shell, long-term memory on, and the coding preset through the same builder call.

#![cfg(all(feature = "runner", feature = "memory", feature = "sandbox"))]

use crate::common;

use std::sync::Arc;

use adk_core::Agent as _;
use adk_harness::personal::{PERSONAL_SYSTEM_PROMPT, PersonalAgent};
use adk_harness::{
    Harness, HarnessMode, IsolationStatus, Preset, ShellIsolation, coding::CODING_SYSTEM_PROMPT,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use common::{APP, collect, fixture, last_system_text, no_user_file, session_id, user};
use serde_json::json;

#[tokio::test]
async fn the_personal_preset_has_no_repository_prompt_and_no_host_shell() {
    let f = fixture(vec![ScriptedResponse::text("Noted.")], |b| b.preset(Preset::Personal)).await;
    let info = f.harness.agent().info().clone();
    assert!(!info.tools.contains(&"bash".to_string()), "{:?}", info.tools);
    assert!(info.tools.contains(&"write_file".to_string()), "{:?}", info.tools);
    assert!(info.members.is_empty(), "no default members: {:?}", info.members);
    assert_eq!(info.default_mode, HarnessMode::Ask);

    f.send("remember that I like tea").await;
    let system = last_system_text(&f.model);
    assert!(system.starts_with(PERSONAL_SYSTEM_PROMPT), "{system}");
    assert!(system.contains("not a code repository"), "{system}");
    assert!(system.contains("shell: none"), "the environment names no shell: {system}");
    assert!(!system.contains("software engineering"), "{system}");
}

#[tokio::test]
async fn the_personal_preset_offers_bash_only_inside_the_sandbox() {
    let f = fixture(vec![ScriptedResponse::text("ok")], |b| {
        b.preset(Preset::Personal).shell(ShellIsolation::OsSandbox(Default::default()))
    })
    .await;
    let info = f.harness.agent().info().clone();
    match info.isolation {
        IsolationStatus::OsSandbox { .. } => {
            assert!(info.tools.contains(&"bash".to_string()), "{:?}", info.tools);
        }
        // Without an enforcer the sandbox degrades, and the preset never falls back to the
        // host.
        _ => assert!(!info.tools.contains(&"bash".to_string()), "{:?}", info.tools),
    }
}

/// Review findings 3 and 5: inside the sandbox the personal preset's `bash` has no
/// unsandboxed retry, whatever the builder asked for after the preset, and the policy refuses
/// one as turned off. Without an enforcer the preset offers no `bash`; the policy unit test
/// `the_personal_preset_turns_the_unsandboxed_retry_off` covers every host.
#[tokio::test]
async fn the_personal_preset_never_retries_on_the_host() {
    let retry = json!({
        "command": "cat ~/.ssh/id_ed25519",
        "unsandboxed": true,
        "retry_of": "1",
        "reason": "the sandbox denied it",
    });
    let f = fixture(
        vec![ScriptedResponse::function_call("bash", retry), ScriptedResponse::text("ok")],
        |b| {
            b.preset(Preset::Personal)
                .shell(ShellIsolation::OsSandbox(Default::default()))
                .allow_unsandboxed_retry(true)
                .mode(HarnessMode::Auto)
        },
    )
    .await;
    let events = f.send("read my key").await;
    let info = f.harness.agent().info().clone();
    if !matches!(info.isolation, IsolationStatus::OsSandbox { .. }) {
        // No enforcer here: the preset offers no shell at all.
        assert!(!info.tools.contains(&"bash".to_string()), "{:?}", info.tools);
        return;
    }
    let request = f.model.last_request().expect("a request");
    let bash = request.tools.get("bash").expect("bash is declared").to_string();
    assert!(!bash.contains("unsandboxed"), "{bash}");
    let answer = common::response_of(&events, "bash");
    // Refused because the retry is off, not because `retry_of` names no failed call.
    assert!(answer.to_string().contains("turned off"), "the retry is refused: {answer}");
}

/// Review findings 11 and 13: the harness reports no shell when the personal preset offers
/// none, so every surface shows the same.
#[tokio::test]
async fn the_harness_reports_no_shell_when_the_preset_offers_none() {
    let f = fixture(vec![ScriptedResponse::text("ok")], |b| b.preset(Preset::Personal)).await;
    assert_eq!(f.harness.agent().info().shell(), None);
    let f = fixture(vec![ScriptedResponse::text("ok")], |b| b.preset(Preset::Coding)).await;
    let info = f.harness.agent().info().clone();
    assert_eq!(info.shell(), Some(&info.isolation));
}

/// Review finding 14: a second preset replaces what the first set.
#[tokio::test]
async fn a_second_preset_replaces_the_first() {
    let f = fixture(vec![ScriptedResponse::text("ok")], |b| {
        b.preset(Preset::Personal).preset(Preset::Coding)
    })
    .await;
    let info = f.harness.agent().info().clone();
    assert!(info.tools.contains(&"bash".to_string()), "the host shell is back: {:?}", info.tools);
    assert_ne!(
        f.harness.agent().description(),
        adk_harness::personal::PERSONAL_DESCRIPTION,
        "the personal description is gone"
    );
    f.send("hi").await;
    let system = last_system_text(&f.model);
    assert!(system.starts_with(CODING_SYSTEM_PROMPT), "{system}");
    assert!(!system.contains("shell: none"), "{system}");
}

/// Review finding 17: on the builder the personal preset leaves long-term memory to the
/// caller; `PersonalAgent` turns it on.
#[tokio::test]
async fn the_builder_preset_leaves_long_term_memory_to_the_caller() {
    let f = fixture(vec![ScriptedResponse::text("ok")], |b| b.preset(Preset::Personal)).await;
    let tools = &f.harness.agent().info().tools;
    assert!(!tools.contains(&"memory_search".to_string()), "{tools:?}");
}

#[tokio::test]
async fn the_coding_preset_is_the_coding_agent_lowering() {
    let f = fixture(vec![ScriptedResponse::text("ok")], |b| b.preset(Preset::Coding)).await;
    let info = f.harness.agent().info().clone();
    assert!(info.tools.contains(&"bash".to_string()));
    assert!(info.members.is_empty());
    f.send("hi").await;
    assert!(last_system_text(&f.model).starts_with(CODING_SYSTEM_PROMPT));
}

#[tokio::test]
async fn a_personal_agent_keeps_its_files_in_its_workspace_and_nowhere_else() {
    let home = tempfile::tempdir().unwrap();
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "lists/groceries.md", "content": "- tea\n- oats\n" }),
            ),
            ScriptedResponse::function_call(
                "write_file",
                json!({ "path": "../outside.txt", "content": "escaped" }),
            ),
            ScriptedResponse::text("Saved your list."),
        ],
    ));
    let agent = PersonalAgent::builder()
        .model(model.clone())
        .home(home.path())
        .configure(|b| b.project_instructions(no_user_file()).mode(HarnessMode::Auto))
        .build()
        .unwrap();
    let workspace = agent.workspace().root().to_path_buf();
    let agent = agent.into_harness();
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = agent
        .configure_runner(
            Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions),
        )
        .build()
        .unwrap();
    let harness = Harness::with_runner(agent, runner).unwrap();
    let alice = user("alice");
    let session = harness.open_session(&alice, Some(&session_id("s1"))).await.unwrap();
    let events =
        collect(harness.send(&alice, &session, "make a grocery list").await.unwrap()).await;

    assert_eq!(workspace, home.path().join("workspace"));
    assert_eq!(
        std::fs::read_to_string(workspace.join("lists/groceries.md")).unwrap(),
        "- tea\n- oats\n"
    );
    assert!(!home.path().join("outside.txt").exists(), "a file outside the workspace");
    let escaped = common::responses(&events)
        .into_iter()
        .filter(|(name, _)| name == "write_file")
        .nth(1)
        .map(|(_, response)| response)
        .expect("the second write_file answered");
    assert!(escaped.get("error").is_some(), "{escaped}");
}

#[tokio::test]
async fn a_personal_agent_has_long_term_memory_on() {
    let home = tempfile::tempdir().unwrap();
    let model = Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("ok")]));
    let on = PersonalAgent::builder()
        .model(model.clone())
        .home(home.path())
        .configure(|b| b.project_instructions(no_user_file()))
        .build()
        .unwrap();
    let tools = &on.harness().info().tools;
    assert!(tools.contains(&"memory_search".to_string()), "{tools:?}");
    assert!(tools.contains(&"memory_remember".to_string()), "{tools:?}");

    let off = PersonalAgent::builder()
        .model(model)
        .home(home.path())
        .without_long_term_memory()
        .configure(|b| b.project_instructions(no_user_file()))
        .build()
        .unwrap();
    assert!(!off.harness().info().tools.contains(&"memory_search".to_string()));
}

#[test]
fn a_personal_agent_needs_a_home() {
    let model = Arc::new(ScriptedLlm::new("scripted", vec![ScriptedResponse::text("ok")]));
    let error = PersonalAgent::builder().model(model).build().err().expect("an error");
    assert!(error.message.contains("home"), "{}", error.message);
}
