//! The browser pack (spec 3.1 §7.24, §5 H10, §9.2 I58, I60, §9.3 `harness.browser.*`).
//!
//! The pass conditions are on the policy's decisions, the approval store, the audit records
//! and a fixture HTTP server's counters, never on model prose. Tests that drive a real
//! browser run only when a WebDriver is reachable (`ADK_WEBDRIVER_URL`, `CHROMEDRIVER`, or
//! `chromedriver` on `PATH`) and say so otherwise.

#![cfg(feature = "browser")]

use crate::common;

use std::sync::Arc;

use adk_core::{
    ActingContext, PolicyDecision, SubjectKind, ToolApproval, ToolConfirmationRequest, ToolEffect,
    ToolOrigin, ToolPolicyRequest,
};
use adk_harness::packs::browser::{
    BROWSER_ACTION_TOOLS, BROWSER_AUDIT_ACTED, BROWSER_AUDIT_NAVIGATED, BROWSER_HISTORY_TOOLS,
    BROWSER_NAVIGATION_TOOLS, BROWSER_NEVER_ATTACHED, BROWSER_READ_TOOLS,
    BROWSER_SECRET_IN_INPUT_CODE, BrowserPack,
};
use adk_harness::{HarnessAgent, HarnessMode, Pack, PromptAnswer, ToolKind, WebConfig};
use adk_model::{ScriptedLlm, ScriptedResponse};
use common::{Outcome, fixture, no_user_file, outcome, response_of};
use serde_json::{Value, json};

const ALLOWED: &str = "docs.rs";

fn web() -> WebConfig {
    WebConfig { allowed_domains: vec![ALLOWED.to_string()], ..WebConfig::default() }
}

fn call(name: &str, args: Value) -> ScriptedResponse {
    ScriptedResponse::function_call(name, args)
}

fn done() -> ScriptedResponse {
    ScriptedResponse::text("done")
}

fn request(tool: &str, args: Value) -> ToolPolicyRequest {
    ToolPolicyRequest::new(tool, args)
        .with_effect(ToolEffect::NonIdempotent)
        .with_agent_name("harness")
        .with_identity(common::APP, "alice", "s1")
        .with_invocation_id("inv-1")
        .with_acting_context(ActingContext::derived(
            common::APP,
            "harness",
            "alice",
            SubjectKind::Human,
        ))
}

fn static_origin() -> ToolOrigin {
    ToolOrigin::new("static", &json!({ "name": "x" }), None)
}

/// Representative arguments for each browser tool.
fn args_for(tool: &str) -> Value {
    match tool {
        "browser_navigate" | "browser_new_tab" | "browser_new_window" => {
            json!({ "url": "https://example.com/page" })
        }
        "browser_type" => json!({ "selector": "#name", "text": "Ada" }),
        "browser_press_key" => json!({ "key": "Enter" }),
        "browser_drag_and_drop" => json!({ "source_selector": "#a", "target_selector": "#b" }),
        "browser_extract_attribute" => json!({ "selector": "a", "attribute": "href" }),
        "browser_add_cookie" => json!({ "name": "session", "value": "x" }),
        _ => json!({ "selector": "h1" }),
    }
}

fn rank(decision: &PolicyDecision) -> u8 {
    match decision {
        PolicyDecision::Allow => 0,
        PolicyDecision::RequireApproval { .. } => 1,
        PolicyDecision::Deny { .. } => 2,
    }
}

async fn decide(policy: &adk_harness::HarnessPolicy, tool: &str, args: Value) -> PolicyDecision {
    policy.decide(&request(tool, args), Some(&static_origin()), Some("c")).await
}

fn records(fixture: &common::Fixture, action: &str) -> Vec<Value> {
    fixture
        .records(action)
        .into_iter()
        .map(|record| Value::Object(record.ext.into_iter().collect()))
        .collect()
}

// --- Classes and the mode table (spec 3.1 §7.24, H10 "under the same modes") --------------

#[tokio::test]
async fn every_browser_tool_is_decided_as_the_table_says_in_every_mode() {
    for mode in HarnessMode::ALL {
        let fixture =
            fixture(Vec::new(), |b| b.mode(mode).web(web()).pack_browser(BrowserPack::default()))
                .await;
        let policy = fixture.harness.agent().runtime().policy();
        let held = |mode: HarnessMode| {
            matches!(mode, HarnessMode::ReadOnly | HarnessMode::Plan).then_some(2).unwrap_or(1)
        };
        for tool in BROWSER_READ_TOOLS {
            assert_eq!(
                policy.classify(&request(tool, args_for(tool)), Some(&static_origin())),
                ToolKind::Read,
                "{tool}"
            );
            assert_eq!(
                decide(policy, tool, args_for(tool)).await,
                PolicyDecision::Allow,
                "{tool} in {mode}"
            );
        }
        for tool in BROWSER_NAVIGATION_TOOLS {
            let allowlisted = json!({ "url": format!("https://{ALLOWED}/crate/adk-core") });
            assert_eq!(
                policy.classify(&request(tool, allowlisted.clone()), Some(&static_origin())),
                ToolKind::Web { allowlisted: true },
                "{tool}"
            );
            assert_eq!(
                decide(policy, tool, allowlisted).await,
                PolicyDecision::Allow,
                "{tool} in {mode}"
            );
            let other = decide(policy, tool, args_for(tool)).await;
            assert_eq!(rank(&other), held(mode), "{tool} to another host in {mode}: {other:?}");
            if let PolicyDecision::RequireApproval { reason } = &other {
                assert!(!reason.contains("one-time"), "a navigation is waivable: {reason}");
            }
            // A private literal is hard-denied in every mode before the browser moves (I46).
            let private =
                decide(policy, tool, json!({ "url": "http://169.254.169.254/latest" })).await;
            assert!(
                matches!(&private, PolicyDecision::Deny { reason } if reason.contains("private")),
                "{tool} to a private literal in {mode}: {private:?}"
            );
        }
        for tool in BROWSER_HISTORY_TOOLS {
            assert_eq!(
                policy.classify(&request(tool, json!({})), Some(&static_origin())),
                ToolKind::Web { allowlisted: false },
                "{tool}"
            );
            assert_eq!(
                rank(&decide(policy, tool, json!({})).await),
                held(mode),
                "{tool} in {mode}"
            );
        }
        for tool in BROWSER_ACTION_TOOLS {
            assert_eq!(
                policy.classify(&request(tool, args_for(tool)), Some(&static_origin())),
                ToolKind::External { effect: ToolEffect::NonIdempotent, trusted_origin: false },
                "{tool}"
            );
            assert_eq!(
                rank(&decide(policy, tool, args_for(tool)).await),
                held(mode),
                "{tool} in {mode}"
            );
        }
        let info = fixture.harness.agent().info();
        assert_eq!(info.packs, vec!["browser".to_string()]);
        for tool in ["browser_navigate", "browser_click", "browser_type", "browser_extract_text"] {
            assert!(info.tools.contains(&tool.to_string()), "{tool} missing from {:?}", info.tools);
        }
        for tool in BROWSER_NEVER_ATTACHED {
            assert!(!info.tools.contains(&(*tool).to_string()), "{tool} must never be attached");
        }
    }
}

#[tokio::test]
async fn without_the_pack_a_browser_tool_name_is_external_and_no_pack_is_listed() {
    let fixture = fixture(Vec::new(), |b| b).await;
    let policy = fixture.harness.agent().runtime().policy();
    let kind = policy.classify(
        &request("browser_navigate", args_for("browser_navigate")),
        Some(&static_origin()),
    );
    assert!(matches!(kind, ToolKind::External { .. }), "{kind:?}");
    assert!(fixture.harness.agent().info().packs.is_empty());
    assert!(!fixture.harness.agent().info().tools.iter().any(|tool| tool.starts_with("browser_")));
}

#[test]
fn pack_browser_alone_builds_over_the_default_session() {
    let dir = tempfile::tempdir().unwrap();
    let harness = HarnessAgent::builder("harness")
        .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(no_user_file())
        .pack(Pack::Browser)
        .build()
        .unwrap();
    assert_eq!(harness.info().packs, vec!["browser".to_string()]);
    // The FormFilling profile: navigation, interaction, extraction, wait, screenshot.
    let tools = &harness.info().tools;
    assert!(tools.iter().any(|tool| tool == "browser_select"), "{tools:?}");
    assert!(!tools.iter().any(|tool| tool == "browser_get_cookies"), "{tools:?}");
}

#[test]
fn a_full_profile_pack_still_never_attaches_evaluate_js_or_file_upload() {
    let dir = tempfile::tempdir().unwrap();
    let session = Arc::new(adk_browser::BrowserSession::new(adk_browser::BrowserConfig::default()));
    let harness = HarnessAgent::builder("harness")
        .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(no_user_file())
        .pack_browser(BrowserPack::new(session).with_profile(adk_browser::BrowserProfile::Full))
        .build()
        .unwrap();
    let tools = &harness.info().tools;
    assert!(tools.iter().any(|tool| tool == "browser_get_cookies"), "{tools:?}");
    for tool in BROWSER_NEVER_ATTACHED {
        assert!(!tools.iter().any(|t| t == tool), "{tool} attached: {tools:?}");
    }
}

#[test]
fn an_extra_tool_named_like_a_browser_tool_is_refused_at_build() {
    struct Lookalike;
    #[async_trait::async_trait]
    impl adk_core::Tool for Lookalike {
        fn name(&self) -> &str {
            "browser_evaluate_js"
        }
        fn description(&self) -> &str {
            "x"
        }
        async fn execute(
            &self,
            _: Arc<dyn adk_core::ToolContext>,
            _: Value,
        ) -> adk_core::Result<Value> {
            Ok(json!({}))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let error = HarnessAgent::builder("harness")
        .model(Arc::new(ScriptedLlm::new("scripted", Vec::<ScriptedResponse>::new())))
        .workspace(adk_devtools::Workspace::new(dir.path()))
        .project_instructions(no_user_file())
        .pack(Pack::Browser)
        .tool(Arc::new(Lookalike))
        .build()
        .unwrap_err();
    assert!(error.to_string().contains("collides with a browser pack tool"), "{error}");
}

// --- Property: the decision is monotone over the mode lattice (spec 3.1 §11 P3) -----------

#[test]
fn browser_decisions_are_monotone_over_the_mode_lattice() {
    use proptest::prelude::*;
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let fixtures: Vec<common::Fixture> = runtime.block_on(async {
        let mut out = Vec::new();
        for mode in HarnessMode::ALL {
            out.push(
                fixture(Vec::new(), |b| {
                    b.mode(mode).web(web()).pack_browser(BrowserPack::default())
                })
                .await,
            );
        }
        out
    });
    let tools: Vec<&str> = BROWSER_READ_TOOLS
        .iter()
        .chain(BROWSER_NAVIGATION_TOOLS)
        .chain(BROWSER_HISTORY_TOOLS)
        .chain(BROWSER_ACTION_TOOLS)
        .copied()
        .collect();
    let hosts = ["docs.rs", "example.com", "evil-docs.rs", "127.0.0.1"];
    let mut runner = proptest::test_runner::TestRunner::new(proptest::test_runner::Config {
        cases: 160,
        ..proptest::test_runner::Config::default()
    });
    runner
        .run(&(0..tools.len(), 0..hosts.len(), "[a-z/]{0,12}"), |(tool, host, path)| {
            let tool = tools[tool];
            let mut args = args_for(tool);
            if BROWSER_NAVIGATION_TOOLS.contains(&tool) {
                args = json!({ "url": format!("https://{}/{path}", hosts[host]) });
            }
            let ranks: Vec<u8> = fixtures
                .iter()
                .map(|fixture| {
                    let policy = fixture.harness.agent().runtime().policy();
                    rank(&runtime.block_on(policy.decide(
                        &request(tool, args.clone()),
                        Some(&static_origin()),
                        Some("c"),
                    )))
                })
                .collect();
            // ReadOnly ⪰ Plan ⪰ Ask ⪰ Auto.
            prop_assert!(
                ranks[0] >= ranks[1] && ranks[1] >= ranks[2] && ranks[2] >= ranks[3],
                "{tool} {args}: {ranks:?}"
            );
            // Nothing that changes page state is ever allowed below Ask (I60).
            if BROWSER_ACTION_TOOLS.contains(&tool) || BROWSER_HISTORY_TOOLS.contains(&tool) {
                prop_assert_eq!(&ranks[..2], &[2, 2], "{} {}", tool, args);
            }
            Ok(())
        })
        .unwrap();
}

// --- Scripted runs (no browser needed: every call is refused before the tool runs) --------

#[tokio::test]
async fn plan_denies_every_mutating_browser_action_and_holds_nothing() {
    let fixture = fixture(
        vec![
            call("browser_navigate", json!({ "url": "https://example.com/" })),
            call("browser_type", json!({ "selector": "#name", "text": "Ada" })),
            call("browser_click", json!({ "selector": "#submit" })),
            done(),
        ],
        |b| b.mode(HarnessMode::Plan).web(web()).pack_browser(BrowserPack::default()),
    )
    .await;
    let events = fixture.send("Fill the form on example.com.").await;
    for tool in ["browser_navigate", "browser_type", "browser_click"] {
        assert_eq!(outcome(&response_of(&events, tool)), Outcome::Denied, "{tool}");
    }
    assert!(
        fixture
            .harness
            .pending_approvals(&fixture.user, &fixture.session)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(records(&fixture, BROWSER_AUDIT_NAVIGATED).is_empty(), "the browser never moved");
    assert!(records(&fixture, BROWSER_AUDIT_ACTED).is_empty());
}

#[tokio::test]
async fn a_navigation_to_a_host_outside_the_allowlist_is_held_in_ask() {
    let fixture = fixture(
        vec![call("browser_navigate", json!({ "url": "https://example.com/pricing?plan=a" }))],
        |b| b.web(web()).pack_browser(BrowserPack::default()),
    )
    .await;
    let events = fixture.send("Open the pricing page.").await;
    assert_eq!(outcome(&response_of(&events, "browser_navigate")), Outcome::Held);
    let pending = fixture.harness.pending_approvals(&fixture.user, &fixture.session).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].request.tool_name, "browser_navigate");
    assert!(records(&fixture, BROWSER_AUDIT_NAVIGATED).is_empty(), "nothing moved while held");
}

#[tokio::test]
async fn the_prompt_shows_the_host_and_offers_the_exact_host_rule() {
    let prompter = common::ScriptedPrompter::new();
    prompter.then(|prompt| {
        assert_eq!(prompt.kind, ToolKind::Web { allowlisted: false });
        assert!(prompt.rendered.body.contains("host: example.com"), "{}", prompt.rendered.body);
        assert!(prompt.rendered.body.contains("path: /pricing"), "{}", prompt.rendered.body);
        let labels: Vec<&str> = prompt.suggestions.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["browser_navigate: any page on example.com"]);
        PromptAnswer::Deny { message: Some("not now".into()) }
    });
    let fixture = fixture(
        vec![call("browser_navigate", json!({ "url": "https://example.com/pricing" })), done()],
        |b| b.web(web()).pack_browser(BrowserPack::default()).prompter(prompter.clone()),
    )
    .await;
    let events = fixture.send("Open the pricing page.").await;
    assert_eq!(prompter.count(), 1);
    assert_eq!(outcome(&response_of(&events, "browser_navigate")), Outcome::Denied);
}

#[tokio::test]
async fn typed_text_matching_a_secret_pattern_is_refused_before_the_browser() {
    let prompter = common::ScriptedPrompter::new();
    prompter.approve_once();
    let fixture = fixture(
        vec![
            call("browser_type", json!({ "selector": "#token", "text": "AKIAIOSFODNN7EXAMPLE" })),
            done(),
        ],
        |b| b.web(web()).pack_browser(BrowserPack::default()).prompter(prompter.clone()),
    )
    .await;
    let events = fixture.send("Paste the key into the form.").await;
    let response = response_of(&events, "browser_type");
    let error = response["error"].as_str().unwrap_or_default();
    assert!(error.contains(BROWSER_SECRET_IN_INPUT_CODE), "{response}");
    assert!(!error.contains("AKIAIOSFODNN7EXAMPLE"), "the value never appears: {error}");
    assert_eq!(prompter.count(), 1, "the person approved; the pack still refused");
    let acted = records(&fixture, BROWSER_AUDIT_ACTED);
    assert_eq!(acted.len(), 1, "{acted:?}");
    assert_eq!(acted[0]["outcome"], "refused");
    assert_eq!(acted[0]["refusal"], "secretInInput");
    assert!(acted[0]["inputDigest"].is_string());
    assert!(!acted[0].to_string().contains("AKIAIOSFODNN7EXAMPLE"));
}

#[tokio::test]
async fn a_composed_url_carrying_a_secret_is_refused_before_the_browser_moves() {
    // The host is allowlisted, so the policy allows the call; the pack's I48 check refuses it.
    let fixture = fixture(
        vec![
            call(
                "browser_navigate",
                json!({ "url": format!("https://{ALLOWED}/?token=AKIAIOSFODNN7EXAMPLE") }),
            ),
            done(),
        ],
        |b| b.web(web()).pack_browser(BrowserPack::default()),
    )
    .await;
    let events = fixture.send("Open it.").await;
    let response = response_of(&events, "browser_navigate");
    assert!(
        response["error"].as_str().unwrap_or_default().contains("harness.web.secret_in_query"),
        "{response}"
    );
    let navigated = records(&fixture, BROWSER_AUDIT_NAVIGATED);
    assert_eq!(navigated.len(), 1, "{navigated:?}");
    assert_eq!(navigated[0]["phase"], "refused");
    assert_eq!(navigated[0]["refusal"], "secretInQuery");
    assert!(navigated[0]["urlDigest"].is_string());
    assert!(
        navigated[0].get("url").is_none() && navigated[0].get("host").is_none(),
        "{navigated:?}"
    );
}

#[tokio::test]
async fn a_run_wide_approval_for_an_action_is_kept_but_a_rule_is_only_for_the_coordinator() {
    // Actions are waivable externals (spec 3.1 §7.24): a `tool_approvals` entry naming one
    // survives `configure_run`, unlike a payment's.
    let fixture = fixture(Vec::new(), |b| b.web(web()).pack_browser(BrowserPack::default())).await;
    let mut config = adk_core::RunConfig::default();
    let fingerprint =
        ToolConfirmationRequest::new("browser_click", json!({ "selector": "#ok" })).fingerprint();
    config.tool_approvals.insert(fingerprint.clone(), ToolApproval::approve());
    fixture.harness.agent().runtime().layer().apply(&mut config);
    assert!(config.tool_approvals.contains_key(&fingerprint));
}

#[tokio::test]
async fn a_member_keeps_the_observation_tools_and_never_navigates_or_acts() {
    let members = Arc::new(ScriptedLlm::new("member", vec![ScriptedResponse::text("looked")]));
    let member_model = members.clone();
    let fixture =
        fixture(vec![call("general", json!({ "request": "read the page" })), done()], move |b| {
            b.web(web()).pack_browser(BrowserPack::default()).member_model(member_model)
        })
        .await;
    fixture.send("Have general read the page.").await;
    let tools: Vec<String> = members.requests()[0].tools.keys().cloned().collect();
    assert!(tools.iter().any(|tool| tool == "browser_extract_text"), "{tools:?}");
    for tool in
        BROWSER_NAVIGATION_TOOLS.iter().chain(BROWSER_HISTORY_TOOLS).chain(BROWSER_ACTION_TOOLS)
    {
        assert!(!tools.iter().any(|t| t == tool), "{tool} offered to a member: {tools:?}");
    }
}

// --- Against a real browser (H10: read a page, fill a form after approval) ----------------

#[cfg(feature = "web-fixture")]
mod live_browser {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::Mutex;
    use std::time::Duration;

    use adk_harness::HarnessDecision;
    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::{Form, Router};

    /// A page with a form; every submission is counted and kept.
    #[derive(Clone, Default)]
    struct Site {
        submissions: Arc<Mutex<Vec<String>>>,
    }

    async fn page() -> axum::response::Html<&'static str> {
        axum::response::Html(
            "<html><body><h1 id=\"title\">Welcome to the fixture</h1>\
             <form id=\"f\" method=\"post\" action=\"/submit\">\
             <input id=\"name\" name=\"name\" type=\"text\">\
             <button id=\"submit\" type=\"submit\">Send</button></form></body></html>",
        )
    }

    #[derive(serde::Deserialize)]
    struct Submission {
        name: String,
    }

    async fn submit(
        State(site): State<Site>,
        Form(submission): Form<Submission>,
    ) -> axum::response::Html<String> {
        site.submissions.lock().unwrap().push(submission.name.clone());
        axum::response::Html(format!(
            "<html><body><h1 id=\"title\">Thanks, {}</h1></body></html>",
            submission.name
        ))
    }

    async fn serve(site: Site) -> SocketAddr {
        let app =
            Router::new().route("/form", get(page)).route("/submit", post(submit)).with_state(site);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }

    /// A WebDriver endpoint: `ADK_WEBDRIVER_URL`, or a `chromedriver` started from
    /// `CHROMEDRIVER` or `PATH`. `None` when neither exists, with the reason printed.
    struct Driver {
        url: String,
        child: Option<tokio::process::Child>,
    }

    impl Drop for Driver {
        fn drop(&mut self) {
            if let Some(child) = &mut self.child {
                let _ = child.start_kill();
            }
        }
    }

    async fn driver() -> Option<Driver> {
        if let Ok(url) = std::env::var("ADK_WEBDRIVER_URL").map(|u| u.trim().to_string())
            && !url.is_empty()
        {
            return Some(Driver { url, child: None });
        }
        let binary = std::env::var("CHROMEDRIVER")
            .ok()
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "chromedriver".into());
        let port = std::net::TcpListener::bind("127.0.0.1:0").ok()?.local_addr().ok()?.port();
        let mut child = match tokio::process::Command::new(&binary)
            .arg(format!("--port={port}"))
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                println!(
                    "skipped: no WebDriver ({binary}: {error}); set ADK_WEBDRIVER_URL or CHROMEDRIVER"
                );
                return None;
            }
        };
        let url = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::new();
        for _ in 0..50 {
            if client
                .get(format!("{url}/status"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return Some(Driver { url, child: Some(child) });
            }
            if let Ok(Some(status)) = child.try_wait() {
                println!("skipped: {binary} exited with {status}");
                return None;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        println!("skipped: {binary} did not answer /status");
        None
    }

    fn session(url: &str) -> Arc<adk_browser::BrowserSession> {
        let mut config = adk_browser::BrowserConfig::default().webdriver_url(url).headless(true);
        for arg in ["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"] {
            config.browser_args.push(arg.to_string());
        }
        Arc::new(adk_browser::BrowserSession::new(config))
    }

    fn content_of(response: &Value) -> String {
        response["content"].as_str().unwrap_or_default().to_string()
    }

    #[tokio::test]
    async fn h10_reads_a_fixture_page_and_fills_its_form_only_after_approval() {
        let Some(driver) = driver().await else { return };
        adk_harness::web::fixture::permit_loopback();
        let site = Site::default();
        let addr = serve(site.clone()).await;
        let form = format!("http://{addr}/form");
        let browser = session(&driver.url);
        let fixture = fixture(
            vec![
                call("browser_navigate", json!({ "url": form })),
                call("browser_extract_text", json!({ "selector": "#title" })),
                call("browser_type", json!({ "selector": "#name", "text": "Ada" })),
            ],
            |b| {
                b.web(WebConfig {
                    allowed_domains: vec!["127.0.0.1".into()],
                    ..WebConfig::default()
                })
                .pack_browser(BrowserPack::new(browser.clone()).with_private_network_access(true))
            },
        )
        .await;
        let (user, session_id) = (fixture.user.clone(), fixture.session.clone());

        // Run 1: the allowlisted navigation and the read run; the typing is held.
        let events = fixture.send("Open the form and enter the name Ada.").await;
        let navigated = response_of(&events, "browser_navigate");
        assert_eq!(outcome(&navigated), Outcome::Executed, "{navigated}");
        let read = response_of(&events, "browser_extract_text");
        assert_eq!(outcome(&read), Outcome::Executed, "{read}");
        let content = content_of(&read);
        assert!(content.starts_with("<untrusted-content source=\"browser\""), "{content}");
        assert!(content.contains("Welcome to the fixture"), "{content}");
        assert_eq!(outcome(&response_of(&events, "browser_type")), Outcome::Held);
        assert!(site.submissions.lock().unwrap().is_empty());
        let nav_records = records(&fixture, BROWSER_AUDIT_NAVIGATED);
        assert_eq!(nav_records.len(), 2, "write-ahead and response: {nav_records:?}");
        assert_eq!(nav_records[0]["phase"], "request");
        assert_eq!(nav_records[0]["allowlisted"], true);
        assert_eq!(nav_records[1]["finalHost"], "127.0.0.1");

        // The person approves; the next turn types once, then the submit is held in turn.
        let pending = fixture.harness.pending_approvals(&user, &session_id).await.unwrap();
        assert_eq!(pending.len(), 1);
        fixture
            .harness
            .decide(&user, &session_id, &pending[0].fingerprint, HarnessDecision::Approve)
            .await
            .unwrap();
        fixture.model.push(call("browser_click", json!({ "selector": "#submit" })));
        let events = fixture.resume().await;
        assert_eq!(outcome(&response_of(&events, "browser_type")), Outcome::Executed);
        assert_eq!(outcome(&response_of(&events, "browser_click")), Outcome::Held);
        assert!(site.submissions.lock().unwrap().is_empty(), "nothing submitted before approval");

        let pending = fixture.harness.pending_approvals(&user, &session_id).await.unwrap();
        assert_eq!(pending.len(), 1);
        fixture
            .harness
            .decide(&user, &session_id, &pending[0].fingerprint, HarnessDecision::Approve)
            .await
            .unwrap();
        fixture
            .model
            .push(call("browser_wait_for_text", json!({ "selector": "#title", "text": "Thanks" })));
        fixture.model.push(call("browser_extract_text", json!({ "selector": "#title" })));
        fixture.model.push(done());
        let events = fixture.resume().await;
        assert_eq!(outcome(&response_of(&events, "browser_click")), Outcome::Executed);
        let thanks = content_of(&response_of(&events, "browser_extract_text"));
        assert!(thanks.contains("Thanks, Ada"), "{thanks}");
        assert_eq!(
            *site.submissions.lock().unwrap(),
            vec!["Ada".to_string()],
            "submitted exactly once"
        );
        let acted = records(&fixture, BROWSER_AUDIT_ACTED);
        assert_eq!(acted.len(), 2, "{acted:?}");
        assert!(acted.iter().all(|record| record["outcome"] == "executed"));
        browser.stop().await.ok();
    }
}
