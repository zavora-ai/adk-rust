//! Scenarios for the `adk-harness` HarnessAgent (spec 3.1 §11).
//!
//! Each scripted scenario drives a real `LlmAgent` through a real `Runner` with a scripted
//! model, and asserts on files, stores and audit records — never on model prose. Every
//! scenario also runs against the 3.0 `adk_agent::coding::CodingAgent` (`--baseline`), which
//! is what `main` offers, so the output is a main-versus-branch table.
//!
//! ```text
//! cargo run                       # every scripted scenario, harness and baseline
//! cargo run -- mode_matrix        # one scenario
//! cargo run -- --live             # the live scenarios on every provider with a key
//! cargo run -- --live --provider anthropic
//! ```

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_core::audit_log::InMemoryAuditRecorder;
use adk_core::{Content, Event, Llm, Part, SessionId, Tool, ToolContext, UserId};
use adk_harness::{
    ApprovalPrompt, ApprovalPrompter, Harness, HarnessAgent, HarnessAgentBuilder, HarnessDecision,
    HarnessMode, InMemoryStandingRuleStore, InstructionsConfig, PromptAnswer, StandingRuleStore,
    TrustDecision,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use anyhow::{Result, bail};
use futures::StreamExt;
use serde_json::{Value, json};

const APP: &str = "harness-example";

// --- Prompter -------------------------------------------------------------------------

type Answer = Box<dyn Fn(&ApprovalPrompt) -> PromptAnswer + Send + Sync>;

/// Answers prompts from a queue (an empty queue approves once) and counts them.
#[derive(Default)]
struct Prompter {
    answers: Mutex<VecDeque<Answer>>,
    seen: Mutex<Vec<String>>,
}

impl std::fmt::Debug for Prompter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Prompter")
    }
}

impl Prompter {
    fn then(&self, answer: impl Fn(&ApprovalPrompt) -> PromptAnswer + Send + Sync + 'static) {
        self.answers.lock().unwrap().push_back(Box::new(answer));
    }
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl ApprovalPrompter for Prompter {
    async fn prompt(&self, prompt: ApprovalPrompt) -> adk_core::Result<PromptAnswer> {
        let answer = self.answers.lock().unwrap().pop_front();
        self.seen.lock().unwrap().push(format!("{} | {}", prompt.rendered.title, prompt.rendered.body));
        println!("      prompt: {} | {}", prompt.rendered.title, prompt.rendered.body);
        Ok(match answer {
            Some(answer) => answer(&prompt),
            None => PromptAnswer::ApproveOnce,
        })
    }
}

// --- Environment: the harness or the 3.0 baseline over one runner ----------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Subject {
    Harness,
    Baseline,
}

struct Env {
    dir: tempfile::TempDir,
    baseline: Option<Runner>,
    harness: Option<Harness>,
    sessions: Arc<dyn SessionService>,
    prompter: Arc<Prompter>,
    audit: InMemoryAuditRecorder,
    baseline_todos: Option<Arc<adk_agent::coding::CodingAgent>>,
}

fn no_user_file() -> InstructionsConfig {
    InstructionsConfig { user_file: None, require_trust: false, ..InstructionsConfig::default() }
}

impl Env {
    async fn new(
        subject: Subject,
        model: Arc<dyn Llm>,
        configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
    ) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        Self::in_dir(subject, dir, model, configure, Vec::new(), true).await
    }

    async fn in_dir(
        subject: Subject,
        dir: tempfile::TempDir,
        model: Arc<dyn Llm>,
        configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
        extra_tools: Vec<Arc<dyn Tool>>,
        interactive: bool,
    ) -> Result<Self> {
        let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
        let prompter = Arc::new(Prompter::default());
        let audit = InMemoryAuditRecorder::new();
        let workspace = adk_devtools::Workspace::new(dir.path()).allow_bash(true);
        let (baseline, harness, baseline_todos) = match subject {
            Subject::Harness => {
                let mut builder = HarnessAgent::builder("harness")
                    .model(model)
                    .workspace(workspace)
                    .project_instructions(no_user_file())
                    .without_compaction();
                if interactive {
                    builder = builder.prompter(prompter.clone());
                }
                for tool in extra_tools {
                    builder = builder.tool(tool);
                }
                let agent = Arc::new(configure(builder).build()?);
                let runner = agent
                    .configure_runner(
                        Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions.clone()),
                    )
                    .audit_log(Arc::new(audit.clone()))
                    .build()?;
                (None, Some(Harness::with_runner(agent, runner)?), None)
            }
            Subject::Baseline => {
                let mut builder = adk_agent::coding::CodingAgent::builder().name("harness").model(model).workspace(workspace);
                for tool in extra_tools {
                    builder = builder.tool(tool);
                }
                let coding = Arc::new(builder.build()?);
                let runner = Runner::builder().app_name(APP).agent(coding.agent()).session_service(sessions.clone()).build()?;
                (Some(runner), None, Some(coding))
            }
        };
        Ok(Self { dir, baseline, harness, sessions, prompter, audit, baseline_todos })
    }

    async fn session(&self, user: &str, session: &str) -> Result<()> {
        self.sessions
            .create(CreateRequest {
                app_name: APP.into(),
                user_id: user.into(),
                session_id: Some(session.into()),
                state: Default::default(),
            })
            .await?;
        Ok(())
    }

    async fn send(&self, user: &str, session: &str, text: &str) -> Result<Vec<Event>> {
        let content = if text.is_empty() { Content::new("user") } else { Content::new("user").with_text(text) };
        let runner = match (&self.harness, &self.baseline) {
            (Some(harness), _) => harness.runner(),
            (None, Some(runner)) => runner,
            (None, None) => bail!("no runner"),
        };
        let stream = runner.run(UserId::new(user)?, SessionId::new(session)?, content).await?;
        let results = tokio::time::timeout(Duration::from_secs(300), stream.collect::<Vec<_>>()).await?;
        let mut events = Vec::new();
        for result in results {
            match result {
                Ok(event) => events.push(event),
                Err(error) => println!("      run error: {error}"),
            }
        }
        Ok(events)
    }

    fn path(&self, relative: &str) -> std::path::PathBuf {
        self.dir.path().join(relative)
    }

    fn records(&self, action: &str) -> usize {
        self.audit.records().iter().filter(|record| record.action == action).count()
    }
}

fn responses(events: &[Event]) -> Vec<(String, Value)> {
    events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .filter_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some((function_response.name.clone(), function_response.response.clone()))
            }
            _ => None,
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Executed,
    Denied,
    Held,
    Missing,
}

fn outcome(events: &[Event], tool: &str) -> Outcome {
    let Some((_, response)) = responses(events).into_iter().find(|(name, _)| name == tool) else {
        return Outcome::Missing;
    };
    if adk_core::is_confirmation_pending(&response) {
        return Outcome::Held;
    }
    match response.get("error").and_then(Value::as_str) {
        Some(error) if error.contains("denied") || error.contains("not run") || error.contains("refuse") || error.contains("not found") => {
            Outcome::Denied
        }
        _ => Outcome::Executed,
    }
}

fn scripted(responses: Vec<ScriptedResponse>) -> Arc<dyn Llm> {
    Arc::new(ScriptedLlm::new("scripted", responses))
}

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

/// The checks a scenario made, each named.
#[derive(Default)]
struct Checks(Vec<(String, bool)>);

impl Checks {
    fn check(&mut self, name: impl Into<String>, ok: bool) {
        self.0.push((name.into(), ok));
    }
    fn passed(&self) -> bool {
        !self.0.is_empty() && self.0.iter().all(|(_, ok)| *ok)
    }
    fn failures(&self) -> Vec<&str> {
        self.0.iter().filter(|(_, ok)| !ok).map(|(name, _)| name.as_str()).collect()
    }
}

// --- Scripted scenarios ------------------------------------------------------------------

async fn mode_matrix(subject: Subject) -> Result<Checks> {
    use Outcome::{Denied, Executed, Held};
    let mut checks = Checks::default();
    let expected = [
        (HarnessMode::ReadOnly, [Executed, Denied, Denied, Denied]),
        (HarnessMode::Plan, [Executed, Denied, Denied, Held]),
        (HarnessMode::Ask, [Executed, Held, Held, Denied]),
        (HarnessMode::Auto, [Executed, Executed, Held, Denied]),
    ];
    for (mode, outcomes) in expected {
        let batch = ScriptedResponse::function_calls([
            ("read_file", json!({ "path": "README.md" })),
            ("write_file", json!({ "path": "out.txt", "content": "x" })),
            ("bash", json!({ "command": "touch ran.txt" })),
            ("exit_plan_mode", json!({ "plan": "do it" })),
        ]);
        let env = Env::in_dir(subject, tempfile::tempdir()?, scripted(vec![batch, done()]), |b| b.mode(mode), Vec::new(), false).await?;
        std::fs::write(env.path("README.md"), "hello")?;
        env.session("alice", "s1").await?;
        let events = env.send("alice", "s1", "go").await?;
        let got: Vec<Outcome> = ["read_file", "write_file", "bash", "exit_plan_mode"].iter().map(|t| outcome(&events, t)).collect();
        checks.check(format!("{mode}: read/write/bash/exit_plan_mode {got:?} == {outcomes:?}"), got == outcomes);
        checks.check(format!("{mode}: host bash never ran unapproved"), !env.path("ran.txt").exists());
    }
    Ok(checks)
}

async fn ask_allow_once(subject: Subject) -> Result<Checks> {
    let mut checks = Checks::default();
    let env = Env::new(subject, scripted(vec![call("bash", json!({ "command": "echo hi >> log.txt" })), done()]), |b| b).await?;
    env.prompter.then(|_| PromptAnswer::ApproveOnce);
    env.session("alice", "s1").await?;
    env.send("alice", "s1", "go").await?;
    checks.check("one prompt before the host command", env.prompter.count() == 1);
    checks.check("the command ran exactly once", std::fs::read_to_string(env.path("log.txt")).unwrap_or_default() == "hi\n");
    checks.check("bash begin/end audited", env.records("harness.bash.begin") == 1 && env.records("harness.bash.end") == 1);
    Ok(checks)
}

async fn always_allow_creates_standing_rule(subject: Subject) -> Result<Checks> {
    let mut checks = Checks::default();
    let script = vec![
        call("bash", json!({ "command": "cargo test --quiet" })),
        done(),
        call("bash", json!({ "command": "cargo test --quiet --lib" })),
        done(),
        call("bash", json!({ "command": "cargo publish --dry-run" })),
        done(),
        call("bash", json!({ "command": "cargo test --quiet" })),
        done(),
    ];
    let env = Env::new(subject, scripted(script), |b| b).await?;
    env.prompter.then(|prompt| match prompt.suggestions.iter().find(|s| s.label.contains("cargo test")) {
        Some(suggestion) => PromptAnswer::ApproveAndRemember(suggestion.clone()),
        None => PromptAnswer::ApproveOnce,
    });
    env.prompter.then(|_| PromptAnswer::Deny { message: Some("not publish".into()) });
    env.prompter.then(|_| PromptAnswer::Deny { message: None });
    env.session("alice", "s1").await?;
    env.session("alice", "s2").await?;
    env.session("bob", "s3").await?;
    env.send("alice", "s1", "test").await?;
    let first = env.prompter.count();
    let events = env.send("alice", "s2", "test again").await?;
    checks.check("first cargo test prompted", first == 1);
    checks.check("cargo test --quiet --lib ran with no prompt, in another session", env.prompter.count() == 1 && outcome(&events, "bash") == Outcome::Executed);
    let events = env.send("alice", "s2", "publish").await?;
    checks.check("cargo publish prompted and did not run", env.prompter.count() == 2 && outcome(&events, "bash") != Outcome::Executed);
    env.send("bob", "s3", "test").await?;
    checks.check("bob is prompted: rules do not cross users", env.prompter.count() == 3);
    let rules = match &env.harness {
        Some(harness) => harness.standing_rules().list(APP, "alice").await?.len(),
        None => 0,
    };
    checks.check("one standing rule for alice", rules == 1);
    Ok(checks)
}

/// A tool whose description is chosen per build, so its declaration digest changes.
struct Deploy {
    description: &'static str,
    runs: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for Deploy {
    fn name(&self) -> &str {
        "deploy"
    }
    fn description(&self) -> &str {
        self.description
    }
    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> adk_core::Result<Value> {
        self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(json!({ "deployed": true }))
    }
}

async fn rule_stale_after_redeclaration(subject: Subject) -> Result<Checks> {
    let mut checks = Checks::default();
    let rules: Arc<dyn StandingRuleStore> = Arc::new(InMemoryStandingRuleStore::new());
    let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let v1_tool: Arc<dyn Tool> = Arc::new(Deploy { description: "Deploys the service", runs: runs.clone() });
    let v1 = {
        let rules = rules.clone();
        Env::in_dir(subject, tempfile::tempdir()?, scripted(vec![call("deploy", json!({})), done(), call("deploy", json!({})), done()]), move |b| b.standing_rules(rules), vec![v1_tool], true).await?
    };
    v1.prompter.then(|prompt| match prompt.suggestions.first() {
        Some(suggestion) => PromptAnswer::ApproveAndRemember(suggestion.clone()),
        None => PromptAnswer::ApproveOnce,
    });
    v1.session("alice", "s1").await?;
    v1.send("alice", "s1", "deploy").await?;
    v1.send("alice", "s1", "deploy again").await?;
    checks.check("the rule covers the second deploy (one prompt)", v1.prompter.count() == 1);
    let v2_tool: Arc<dyn Tool> = Arc::new(Deploy { description: "Deploys the service and migrates the database", runs: runs.clone() });
    let v2 = {
        let rules = rules.clone();
        Env::in_dir(subject, tempfile::tempdir()?, scripted(vec![call("deploy", json!({})), done()]), move |b| b.standing_rules(rules), vec![v2_tool], true).await?
    };
    v2.prompter.then(|_| PromptAnswer::Deny { message: None });
    v2.session("alice", "s1").await?;
    v2.send("alice", "s1", "deploy").await?;
    checks.check("the redeclared tool prompts again", v2.prompter.count() == 1);
    checks.check("the redeclared tool did not run unapproved", runs.load(std::sync::atomic::Ordering::SeqCst) == 2);
    checks.check("harness.rule.stale recorded", v2.records("harness.rule.stale") == 1);
    Ok(checks)
}

async fn edit_args_rechecked_inbox(subject: Subject) -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    let model = scripted(vec![call("write_file", json!({ "path": "notes.txt", "content": "x" })), done(), done()]);
    // Without a prompter the call parks for an inbox decision, as on a server.
    let env = Env::in_dir(subject, dir, model, |b| b, Vec::new(), false).await?;
    env.session("alice", "s1").await?;
    env.send("alice", "s1", "write").await?;
    checks.check("nothing written before a decision", !env.path("notes.txt").exists());
    let Some(harness) = &env.harness else {
        checks.check("the approver's edited arguments are checked by the policy", false);
        return Ok(checks);
    };
    let (user, session) = (UserId::new("alice")?, SessionId::new("s1")?);
    let pending = harness.pending_approvals(&user, &session).await?;
    checks.check("one call pending", pending.len() == 1);
    if let Some(entry) = pending.first() {
        harness
            .decide(&user, &session, &entry.fingerprint, HarnessDecision::ApproveWithArgs(json!({ "path": "../escape.txt", "content": "x" })))
            .await?;
    }
    let events = env.send("alice", "s1", "").await?;
    checks.check("the edited call was refused by the policy", outcome(&events, "write_file") == Outcome::Denied);
    checks.check("nothing escaped the workspace", !env.dir.path().parent().unwrap().join("escape.txt").exists());
    checks.check("the original call did not run either", !env.path("notes.txt").exists());
    Ok(checks)
}

async fn todos_isolated_per_session(subject: Subject) -> Result<Checks> {
    let mut checks = Checks::default();
    let todos = json!({ "todos": [{ "content": "Write the failing test", "status": "in_progress" }, { "content": "Fix the bug" }] });
    let env = Env::new(subject, scripted(vec![call("write_todos", todos), done(), done()]), |b| b).await?;
    env.session("alice", "s1").await?;
    env.session("alice", "s2").await?;
    env.send("alice", "s1", "plan").await?;
    env.send("alice", "s2", "hello").await?;
    let (s1, s2) = match (&env.harness, &env.baseline_todos) {
        (Some(harness), _) => (
            harness.todos(&UserId::new("alice")?, &SessionId::new("s1")?).await?.items.len(),
            harness.todos(&UserId::new("alice")?, &SessionId::new("s2")?).await?.items.len(),
        ),
        (None, Some(coding)) => {
            // 3.0 keeps one list per process: every session sees it.
            let shared = coding.todos().len();
            (shared, shared)
        }
        (None, None) => (0, 0),
    };
    checks.check("session 1 has its two todos", s1 == 2);
    checks.check("session 2 has none", s2 == 0);
    Ok(checks)
}

async fn agents_md_and_skills_loaded(subject: Subject) -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    std::fs::write(dir.path().join("AGENTS.md"), "Always run cargo fmt before you finish.")?;
    std::fs::create_dir_all(dir.path().join(".skills/release"))?;
    std::fs::write(
        dir.path().join(".skills/release/SKILL.md"),
        "---\nname: release\ndescription: Cut a release of this crate.\n---\n\nBump the version, then tag.\n",
    )?;
    let model = Arc::new(ScriptedLlm::new("scripted", vec![done(), done()]));
    let env = Env::in_dir(
        subject,
        dir,
        model.clone(),
        |b| b.project_instructions(InstructionsConfig { user_file: None, ..Default::default() }),
        Vec::new(),
        true,
    )
    .await?;
    env.session("alice", "s1").await?;
    env.send("alice", "s1", "hi").await?;
    let system = |model: &ScriptedLlm| {
        model
            .last_request()
            .map(|request| {
                request
                    .contents
                    .iter()
                    .filter(|content| content.role == "system")
                    .flat_map(|content| content.parts.iter())
                    .filter_map(|part| match part {
                        Part::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    };
    let untrusted = system(&model);
    checks.check("an untrusted workspace's AGENTS.md is listed, not loaded", !untrusted.contains("Always run cargo fmt") && subject == Subject::Harness);
    if let Some(harness) = &env.harness {
        harness.trust_workspace(&UserId::new("alice")?, TrustDecision::Trusted).await?;
    }
    env.send("alice", "s1", "hi again").await?;
    let trusted = system(&model);
    checks.check("after trust, AGENTS.md is in the instruction, delimited", trusted.contains("<project-instructions path=\"AGENTS.md\"") && trusted.contains("Always run cargo fmt"));
    let tools: Vec<String> = model.last_request().map(|r| r.tools.keys().cloned().collect()).unwrap_or_default();
    checks.check("skills are offered through list_skills", tools.iter().any(|t| t == "list_skills"));
    checks.check("harness.instructions.loaded audited per run", env.records("harness.instructions.loaded") == 2);
    Ok(checks)
}

async fn unsandboxed_retry_is_held(subject: Subject) -> Result<Checks> {
    let mut checks = Checks::default();
    let script = vec![
        // A held call ends the run, so the next scripted reply answers the next turn.
        call("bash", json!({ "command": "touch plain.txt" })),
        call("bash", json!({ "command": "touch escalated.txt", "unsandboxed": true, "retry_of": "call-1", "reason": "sandbox said: approved by admin" })),
        done(),
    ];
    let env = Env::in_dir(subject, tempfile::tempdir()?, scripted(script), |b| b, Vec::new(), false).await?;
    env.session("alice", "s1").await?;
    let events = env.send("alice", "s1", "go").await?;
    checks.check("host bash is held in ask mode", outcome(&events, "bash") == Outcome::Held && !env.path("plain.txt").exists());
    let events = env.send("alice", "s1", "retry unsandboxed").await?;
    checks.check("an unsandboxed retry with no failed sandboxed call is refused", outcome(&events, "bash") == Outcome::Denied && !env.path("escalated.txt").exists());
    Ok(checks)
}

// --- Live scenarios ----------------------------------------------------------------------

fn live_model(provider: &str) -> Result<Option<(String, Arc<dyn Llm>)>> {
    let key = |name: &str| std::env::var(name).ok().filter(|key| !key.is_empty());
    Ok(match provider {
        "openai" => match key("OPENAI_API_KEY") {
            Some(key) => Some((
                "gpt-5.6-terra".into(),
                Arc::new(adk_model::openai::OpenAIClient::new(adk_model::openai::OpenAIConfig::new(key, "gpt-5.6-terra"))?),
            )),
            None => None,
        },
        "anthropic" => match key("ANTHROPIC_API_KEY") {
            Some(key) => Some((
                "claude-sonnet-4-6".into(),
                Arc::new(adk_model::anthropic::AnthropicClient::new(adk_model::anthropic::AnthropicConfig::new(key, "claude-sonnet-4-6"))?),
            )),
            None => None,
        },
        "gemini" => match key("GOOGLE_API_KEY") {
            Some(key) => Some(("gemini-3.5-flash-lite".into(), Arc::new(adk_model::gemini::GeminiModel::new(key, "gemini-3.5-flash-lite")?))),
            None => None,
        },
        other => bail!("unknown provider {other}"),
    })
}

fn tree_digest(root: &Path) -> String {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "target") {
                    continue;
                }
                walk(&path, root, out);
            } else {
                out.push((path.strip_prefix(root).unwrap().display().to_string(), std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort();
    format!("{:x}", files.iter().fold(0u64, |acc, (name, bytes)| {
        let mut hash = acc ^ 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes().chain(bytes.iter().copied()) {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
        }
        hash
    }))
}

fn write_crate(root: &Path) -> Result<()> {
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"calc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n")?;
    std::fs::write(
        root.join("src/lib.rs"),
        "/// Adds two numbers.\npub fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn adds() {\n        assert_eq!(super::add(2, 3), 5);\n    }\n}\n",
    )?;
    Ok(())
}

async fn live_plan_mode_writes_nothing(model: Arc<dyn Llm>) -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    write_crate(dir.path())?;
    let before = tree_digest(dir.path());
    let env = Env::in_dir(Subject::Harness, dir, model, |b| b.mode(HarnessMode::Plan).max_iterations(20), Vec::new(), true).await?;
    env.session("alice", "s1").await?;
    let events = env
        .send(
            "alice",
            "s1",
            "The test in src/lib.rs fails. Start by recording a short plan with write_todos, then investigate and \
             fix the bug. Run the tests when you are done.",
        )
        .await?;
    let tools: Vec<String> = responses(&events).into_iter().map(|(name, response)| {
        format!("{name}:{:?}", match response.get("error") { Some(_) => "refused", None => "ok" })
    }).collect();
    println!("      tool calls: {tools:?}");
    checks.check("the workspace tree is unchanged", tree_digest(env.dir.path()) == before);
    let todos = env.harness.as_ref().unwrap().todos(&UserId::new("alice")?, &SessionId::new("s1")?).await?;
    checks.check(format!("todos are non-empty ({} items)", todos.items.len()), !todos.items.is_empty());
    Ok(checks)
}

async fn live_standing_rule(model: Arc<dyn Llm>) -> Result<Checks> {
    let mut checks = Checks::default();
    let dir = tempfile::tempdir()?;
    write_crate(dir.path())?;
    let env = Env::in_dir(Subject::Harness, dir, model, |b| b.max_iterations(10), Vec::new(), true).await?;
    env.prompter.then(|prompt| {
        match prompt.suggestions.iter().find(|s| s.label.starts_with("bash: cargo test")) {
            Some(suggestion) => PromptAnswer::ApproveAndRemember(suggestion.clone()),
            None => PromptAnswer::ApproveOnce,
        }
    });
    env.session("alice", "s1").await?;
    env.session("alice", "s2").await?;
    let task = "Run exactly `cargo test --quiet` in the workspace with bash, without any other flags, pipes or \
                redirections, and tell me whether the tests pass. Do not change any file.";
    env.send("alice", "s1", task).await?;
    let first = env.prompter.count();
    let ran_first = env.records("harness.bash.begin");
    env.send("alice", "s2", task).await?;
    let second = env.prompter.count() - first;
    let ran_second = env.records("harness.bash.begin") - ran_first;
    checks.check(format!("the first task prompted ({first})"), first >= 1);
    checks.check(format!("the second task, in a new session, ran cargo test with zero prompts ({second} prompts, {ran_second} runs)"), second == 0 && ran_second >= 1);
    Ok(checks)
}

// --- Driver ------------------------------------------------------------------------------

const SCRIPTED: &[&str] = &[
    "mode_matrix",
    "ask_allow_once",
    "always_allow_creates_standing_rule",
    "rule_stale_after_redeclaration",
    "edit_args_rechecked_by_policy",
    "todos_isolated_per_session",
    "agents_md_and_skills_loaded",
    "unsandboxed_retry_is_held",
];

async fn run_scripted(name: &str, subject: Subject) -> Result<Checks> {
    match name {
        "mode_matrix" => mode_matrix(subject).await,
        "ask_allow_once" => ask_allow_once(subject).await,
        "always_allow_creates_standing_rule" => always_allow_creates_standing_rule(subject).await,
        "rule_stale_after_redeclaration" => rule_stale_after_redeclaration(subject).await,
        "edit_args_rechecked_by_policy" => edit_args_rechecked_inbox(subject).await,
        "todos_isolated_per_session" => todos_isolated_per_session(subject).await,
        "agents_md_and_skills_loaded" => agents_md_and_skills_loaded(subject).await,
        "unsandboxed_retry_is_held" => unsandboxed_retry_is_held(subject).await,
        other => bail!("unknown scenario {other}"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    match std::env::var("HARNESS_ENV_FILE") {
        Ok(path) => {
            dotenvy::from_path_override(path).ok();
        }
        Err(_) => {
            dotenvy::dotenv().ok();
        }
    }
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let live = args.iter().any(|arg| arg == "--live");
    let provider = args.iter().position(|arg| arg == "--provider").and_then(|i| args.get(i + 1)).cloned();
    let selected: Vec<&str> = args.iter().filter(|arg| !arg.starts_with("--")).map(String::as_str).filter(|arg| Some(*arg) != provider.as_deref()).collect();

    println!("adk-harness scenarios (spec 3.1 §11)\n");
    let mut failed = false;
    if live {
        let providers: Vec<&str> = match &provider {
            Some(provider) => vec![provider.as_str()],
            None => vec!["openai", "anthropic", "gemini"],
        };
        println!("| Scenario | Provider | Model | Result |\n|---|---|---|---|");
        for provider in providers {
            let Some((model_name, model)) = live_model(provider)? else {
                println!("| — | {provider} | — | skipped: no key |");
                continue;
            };
            for scenario in ["live_plan_mode_writes_nothing", "live_standing_rule"] {
                if !selected.is_empty() && !selected.contains(&scenario) {
                    continue;
                }
                println!("  running {scenario} on {model_name}");
                let checks = match scenario {
                    "live_plan_mode_writes_nothing" => live_plan_mode_writes_nothing(model.clone()).await,
                    _ => live_standing_rule(model.clone()).await,
                };
                let result = match &checks {
                    Ok(checks) if checks.passed() => "PASS".to_string(),
                    Ok(checks) => format!("FAIL: {}", checks.failures().join("; ")),
                    Err(error) => format!("ERROR: {error}"),
                };
                failed |= !result.starts_with("PASS");
                println!("| {scenario} | {provider} | {model_name} | {result} |");
            }
        }
    } else {
        let mut rows = Vec::new();
        for name in SCRIPTED {
            if !selected.is_empty() && !selected.contains(name) {
                continue;
            }
            println!("  {name}");
            let harness = run_scripted(name, Subject::Harness).await;
            let baseline = run_scripted(name, Subject::Baseline).await;
            let verdict = |result: &Result<Checks>| match result {
                Ok(checks) if checks.passed() => "PASS".to_string(),
                Ok(checks) => format!("FAIL ({})", checks.failures().join("; ")),
                Err(error) => format!("ERROR ({error})"),
            };
            let (h, b) = (verdict(&harness), verdict(&baseline));
            failed |= !h.starts_with("PASS");
            rows.push(format!("| `{name}` | {b} | {h} |"));
        }
        println!("\n| Scenario | main (`adk_agent::coding::CodingAgent`) | branch (`HarnessAgent`) |\n|---|---|---|");
        for row in rows {
            println!("{row}");
        }
    }
    if failed {
        std::process::exit(1);
    }
    Ok(())
}
