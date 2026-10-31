//! Identity through a real harness run (spec 3.1 §7.14, §9.2 I6, I39): RBAC on tool
//! classes, per-call credentials for the shell, authenticated identity on a served harness,
//! the acting context on every record, and the audit trail every run requires.

#![cfg(all(feature = "runner", feature = "auth"))]

use crate::common;

use std::sync::{Arc, Mutex};

use adk_core::audit_log::{AuditFailureMode, AuditTrail, InMemoryAuditRecorder};
use adk_core::{
    ActingContext, CredentialBroker, RequestContext, RevocationScope, RunConfig, ScopedToken, Url,
};
use adk_harness::{
    EphemeralAuditLog, HARNESS_AUDIT_REQUIRED_CODE, Harness, HarnessAgent, HarnessAgentBuilder,
    HarnessMode, RbacMap, ShellCredential,
};
use adk_model::{ScriptedLlm, ScriptedResponse};
use adk_runner::Runner;
use adk_session::{InMemorySessionService, SessionService};
use common::{Outcome, collect, outcome, response_of};
use serde_json::{Value, json};

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

struct Served {
    dir: tempfile::TempDir,
    harness: Harness,
    audit: InMemoryAuditRecorder,
}

/// A harness whose runner serves every run with `request_context`, as `adk-server` does
/// for a request its extractor authenticated.
async fn served(
    responses: Vec<ScriptedResponse>,
    request_context: Option<RequestContext>,
    configure: impl FnOnce(HarnessAgentBuilder) -> HarnessAgentBuilder,
) -> Served {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(ScriptedLlm::new("scripted", responses));
    let builder = HarnessAgent::builder("harness")
        .model(model)
        .workspace(adk_devtools::Workspace::new(dir.path()).allow_bash(true))
        .project_instructions(common::no_user_file())
        .without_compaction();
    let agent = Arc::new(configure(builder).build().unwrap());
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let audit = InMemoryAuditRecorder::new();
    let mut runner = agent
        .configure_runner(
            Runner::builder().app_name(common::APP).agent(agent.clone()).session_service(sessions),
        )
        .audit_log(Arc::new(audit.clone()));
    if let Some(request_context) = request_context {
        runner = runner.request_context(request_context);
    }
    let harness = Harness::with_runner(agent, runner.build().unwrap()).unwrap();
    harness.open_session(&common::user("alice"), Some(&common::session_id("s1"))).await.unwrap();
    Served { dir, harness, audit }
}

impl Served {
    async fn send(&self, text: &str) -> Vec<adk_core::Event> {
        let stream = self
            .harness
            .send(&common::user("alice"), &common::session_id("s1"), text)
            .await
            .unwrap();
        collect(stream).await
    }
}

fn alice(scopes: &[&str]) -> RequestContext {
    RequestContext {
        user_id: "alice".to_string(),
        scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
        metadata: Default::default(),
    }
}

#[tokio::test]
async fn rbac_denies_bash_to_a_user_without_the_shell_permission() {
    let bash = || call("bash", json!({ "command": "echo ran >> ran.txt" }));
    let configure = |b: HarnessAgentBuilder| {
        b.rbac(RbacMap::harness_defaults()).mode(HarnessMode::Auto).allow_host_shell_in_auto()
    };
    let denied = served(vec![bash(), done()], Some(alice(&["harness:edit"])), configure).await;
    let events = denied.send("run it").await;
    let response = response_of(&events, "bash");
    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("harness.rbac.denied:harness:shell"),
        "the denial names the permission: {response}"
    );
    assert!(!denied.dir.path().join("ran.txt").exists(), "the command did not run");

    let allowed = served(vec![bash(), done()], Some(alice(&["harness:shell"])), configure).await;
    let events = allowed.send("run it").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed);
    assert_eq!(std::fs::read_to_string(allowed.dir.path().join("ran.txt")).unwrap(), "ran\n");
}

/// Issues a fixed token per resource and records every mint and revocation.
#[derive(Debug, Default)]
struct RecordingBroker {
    minted: Mutex<Vec<(ActingContext, Url)>>,
    revoked: Mutex<Vec<RevocationScope>>,
}

#[async_trait::async_trait]
impl CredentialBroker for RecordingBroker {
    async fn token_for(
        &self,
        acting: &ActingContext,
        resource: &Url,
        scopes: &[String],
    ) -> adk_core::Result<ScopedToken> {
        self.minted.lock().unwrap().push((acting.clone(), resource.clone()));
        let subject = acting.on_behalf_of.as_ref().map(|s| s.id.clone()).unwrap_or_default();
        Ok(ScopedToken::new(
            acting.agent.id.clone(),
            format!("tok-{subject}-{}", resource.host_str().unwrap_or_default()),
            resource.clone(),
            scopes.to_vec(),
        ))
    }

    async fn revoke(&self, scope: &RevocationScope) -> adk_core::Result<u64> {
        self.revoked.lock().unwrap().push(scope.clone());
        Ok(1)
    }
}

#[tokio::test]
async fn the_shell_gets_a_subject_token_only_for_the_duration_of_one_call() {
    let broker = Arc::new(RecordingBroker::default());
    let script = concat!(
        "printf '%s' \"$GIT_ASKPASS\" > helper-path.txt; ",
        "\"$GIT_ASKPASS\" 'Password for https://x@github.com': > askpass.txt; ",
        "printf 'protocol=https\\nhost=github.com\\n\\n' | \"$ADK_CREDENTIAL_HELPER\" get > helper.txt; ",
        "env > env.txt"
    );
    let handle = broker.clone();
    let assert_absent = || {
        assert!(std::env::var_os("GIT_ASKPASS").is_none());
        assert!(std::env::vars().all(|(_, value)| !value.contains("tok-alice")));
    };
    assert_absent();
    let s = served(
        vec![
            call("bash", json!({ "command": script })),
            call("bash", json!({ "command": "env > after.txt" })),
            done(),
        ],
        Some(alice(&[])),
        move |b| {
            b.credentials(handle)
                .shell_credential(ShellCredential::new("https://github.com/").unwrap())
                .mode(HarnessMode::Auto)
                .allow_host_shell_in_auto()
        },
    )
    .await;
    let events = s.send("push the branch").await;
    assert_eq!(outcome(&response_of(&events, "bash")), Outcome::Executed);
    assert_absent();

    let read = |name: &str| std::fs::read_to_string(s.dir.path().join(name)).unwrap();
    assert_eq!(
        read("askpass.txt").trim(),
        "tok-alice-github.com",
        "GIT_ASKPASS answers git's prompt"
    );
    assert_eq!(read("helper.txt"), "username=x-access-token\npassword=tok-alice-github.com\n");
    let env = read("env.txt");
    assert!(env.contains("GIT_ASKPASS="), "{env}");
    assert!(!env.contains("tok-alice"), "the token is never in the shell's environment");
    let helper = read("helper-path.txt");
    assert!(!std::path::Path::new(&helper).exists(), "the helper is deleted after the call");
    let next = read("after.txt");
    let next_helper = next
        .lines()
        .find_map(|line| line.strip_prefix("GIT_ASKPASS="))
        .expect("the next call gets its own helper");
    assert_ne!(next_helper, helper, "each call gets a fresh helper");
    assert!(!std::path::Path::new(next_helper).exists());

    let minted = broker.minted.lock().unwrap().clone();
    assert_eq!(minted.len(), 2, "one token per call");
    let (acting, resource) = &minted[0];
    assert_eq!(acting.on_behalf_of.as_ref().map(|s| s.id.as_str()), Some("alice"));
    assert_eq!(resource.as_str(), "https://github.com/");
    let revoked = broker.revoked.lock().unwrap().clone();
    assert_eq!(
        revoked,
        vec![
            RevocationScope::AgentResource {
                agent: acting.agent.id.clone(),
                resource: resource.clone()
            };
            2
        ],
        "every token is revoked after its call"
    );
}

#[tokio::test]
async fn standing_rules_and_auto_are_inert_without_authenticated_identity() {
    let write = || call("write_file", json!({ "path": "a.txt", "content": "x" }));
    let configure = |prompter: Arc<common::ScriptedPrompter>| {
        move |b: HarnessAgentBuilder| {
            b.require_authenticated_identity().mode(HarnessMode::Auto).prompter(prompter)
        }
    };
    // Unauthenticated: Auto decides as Ask, so the write asks (and is denied).
    let prompter = common::ScriptedPrompter::new();
    prompter.deny();
    let anonymous = served(vec![write(), done()], None, configure(prompter.clone())).await;
    let events = anonymous.send("write").await;
    assert_eq!(outcome(&response_of(&events, "write_file")), Outcome::Denied);
    assert_eq!(prompter.count(), 1, "Auto was inert");
    assert!(!anonymous.dir.path().join("a.txt").exists());

    // Authenticated: Auto allows the write without asking.
    let prompter = common::ScriptedPrompter::new();
    let authenticated =
        served(vec![write(), done()], Some(alice(&[])), configure(prompter.clone())).await;
    let events = authenticated.send("write").await;
    assert_eq!(outcome(&response_of(&events, "write_file")), Outcome::Executed);
    assert_eq!(prompter.count(), 0);

    // A standing rule applies only to the authenticated run.
    for (request_context, prompts) in [(None, 1), (Some(alice(&[])), 0)] {
        let prompter = common::ScriptedPrompter::new();
        prompter.deny();
        let rules = Arc::new(adk_harness::InMemoryStandingRuleStore::new());
        let handle = prompter.clone();
        let store = rules.clone();
        let s = served(
            vec![call("bash", json!({ "command": "echo ok" })), done()],
            request_context,
            move |b| b.require_authenticated_identity().prompter(handle).standing_rules(store),
        )
        .await;
        let bash = s.harness.agent().info().tools.iter().any(|tool| tool == "bash");
        assert!(bash);
        let suggestion = rule_for_echo(&s).await;
        adk_harness::StandingRuleStore::grant(rules.as_ref(), suggestion).await.unwrap();
        s.send("echo").await;
        assert_eq!(prompter.count(), prompts);
    }
}

/// A user-scope rule allowing `echo` for alice, pinned to this harness's `bash`.
async fn rule_for_echo(s: &Served) -> adk_harness::StandingRule {
    let tools = adk_core::Toolset::tools(
        s.harness.agent().runtime().toolset().as_ref(),
        Arc::new(adk_tool_free_context()),
    )
    .await
    .unwrap();
    let bash = tools.into_iter().find(|tool| tool.name() == "bash").unwrap();
    let origin = adk_core::ToolOrigin::of(bash.as_ref());
    adk_harness::StandingRule {
        id: "echo".into(),
        scope: adk_harness::RuleScope::User {
            app_name: common::APP.into(),
            user_id: "alice".into(),
            subject_kind: adk_core::SubjectKind::Human,
        },
        tool: "bash".into(),
        origin: adk_harness::OriginPin::Exact {
            source: origin.source.clone(),
            declaration_digest: origin.declaration_digest.clone(),
        },
        agent: None,
        predicates: vec![adk_harness::RulePredicate::ShellPrefix {
            pointer: "/command".into(),
            tokens: vec!["echo".into()],
        }],
        decision: adk_harness::StandingDecision::Allow,
        created_by: adk_core::SubjectRef::human("alice"),
        created_at: chrono::Utc::now(),
        expires_at: None,
        label: "echo".into(),
        source_fingerprint: None,
    }
}

fn adk_tool_free_context() -> impl adk_core::ReadonlyContext {
    struct Ctx(adk_core::Content);
    #[async_trait::async_trait]
    impl adk_core::ReadonlyContext for Ctx {
        fn invocation_id(&self) -> &str {
            "probe"
        }
        fn agent_name(&self) -> &str {
            "harness"
        }
        fn user_id(&self) -> &str {
            "alice"
        }
        fn app_name(&self) -> &str {
            common::APP
        }
        fn session_id(&self) -> &str {
            "s1"
        }
        fn branch(&self) -> &str {
            ""
        }
        fn user_content(&self) -> &adk_core::Content {
            &self.0
        }
    }
    Ctx(adk_core::Content::new("user"))
}

#[tokio::test]
async fn every_harness_record_names_the_acting_agent_and_its_subject() {
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let handle = prompter.clone();
    let s = served(
        vec![call("bash", json!({ "command": "echo hi" })), done()],
        Some(alice(&[])),
        move |b| b.prompter(handle),
    )
    .await;
    s.send("hi").await;
    let records = s.audit.records();
    let harness: Vec<_> = records
        .iter()
        .filter(|record| record.action.starts_with("harness.") || record.action == "bash")
        .collect();
    assert!(harness.len() >= 4, "{harness:#?}");
    for record in harness {
        assert_eq!(
            record.actor.agent.as_ref().map(adk_core::AgentId::as_str),
            Some("urn:adk:agent:harness-tests/harness"),
            "{record:#?}"
        );
        assert_eq!(record.actor.on_behalf_of.as_ref().map(|s| s.id.as_str()), Some("alice"));
        assert!(record.actor.invocation_id.is_some());
    }
}

#[tokio::test]
async fn a_run_without_a_blocking_audit_trail_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let model = Arc::new(ScriptedLlm::new("scripted", vec![done()]));
    let agent = Arc::new(
        HarnessAgent::builder("harness")
            .model(model.clone())
            .workspace(adk_devtools::Workspace::new(dir.path()))
            .project_instructions(common::no_user_file())
            .without_compaction()
            .build()
            .unwrap(),
    );
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let warn = RunConfig {
        audit: Some(
            AuditTrail::new(Arc::new(InMemoryAuditRecorder::new()))
                .with_failure_mode(AuditFailureMode::Warn),
        ),
        ..RunConfig::default()
    };
    let runner = Runner::builder()
        .app_name(common::APP)
        .agent(agent.clone())
        .session_service(sessions)
        .run_config(warn)
        .build()
        .unwrap();
    let harness = Harness::with_runner(agent, runner).unwrap();
    let session = harness.open_session(&common::user("alice"), None).await.unwrap();
    let stream = harness.send(&common::user("alice"), &session, "hi").await.unwrap();
    let results: Vec<_> = futures::StreamExt::collect(stream).await;
    let error = results.into_iter().find_map(Result::err).expect("the run is refused");
    assert_eq!(error.code, HARNESS_AUDIT_REQUIRED_CODE);
    assert!(model.requests().is_empty(), "nothing ran");
}

#[tokio::test]
async fn a_plain_runner_records_to_the_harness_signed_log() {
    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(EphemeralAuditLog::new().unwrap());
    let model = Arc::new(ScriptedLlm::new(
        "scripted",
        vec![call("write_file", json!({ "path": "a.txt", "content": "x" })), done()],
    ));
    let agent = Arc::new(
        HarnessAgent::builder("harness")
            .model(model)
            .workspace(adk_devtools::Workspace::new(dir.path()))
            .project_instructions(common::no_user_file())
            .without_compaction()
            .mode(HarnessMode::Auto)
            .audit_log(log.clone())
            .build()
            .unwrap(),
    );
    // No `configure_runner`, no runner audit log: `configure_run` installs the harness's.
    let sessions: Arc<dyn SessionService> = Arc::new(InMemorySessionService::new());
    let runner = Runner::builder()
        .app_name(common::APP)
        .agent(agent.clone())
        .session_service(sessions)
        .build()
        .unwrap();
    let harness = Harness::with_runner(agent, runner).unwrap();
    let session = harness.open_session(&common::user("alice"), None).await.unwrap();
    collect(harness.send(&common::user("alice"), &session, "write").await.unwrap()).await;
    assert!(dir.path().join("a.txt").exists());
    let size = adk_core::audit_log::AuditLogStore::size(log.store().as_ref(), "adk-harness")
        .await
        .unwrap();
    assert!(size >= 2, "the instruction load and the governed write are in the log: {size}");
    assert!(log.verify().await.unwrap(), "the signed log verifies");
}
