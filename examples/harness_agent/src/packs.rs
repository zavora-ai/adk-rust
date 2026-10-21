//! Browser and computer-use packs (spec 3.1 §5 H10, §7.24): browser tools under the same
//! modes, a form filled only after approval, and a desktop action performed once after a
//! digest-bound approval.
//!
//! The browser scenarios drive a headless Chrome through WebDriver against a local fixture
//! page (`ADK_WEBDRIVER_URL`, `CHROMEDRIVER`, or `chromedriver` on `PATH`; skipped otherwise).
//! The desktop runtime is an in-process double of adk-computer-use's `ComputerUseRuntime`
//! that counts every mutation. The 3.0 `CodingAgent` gets the same `BrowserToolset` as plain
//! tools, which is what `main` offers, so the baseline column shows a form submitted with
//! nobody asked.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_browser::{BrowserConfig, BrowserProfile, BrowserSession, BrowserToolset};
use adk_computer_use::{
    ActionClass, ActionEnvelope, ActionPostcondition, ActionPreview, ComputerUseError, ComputerUseRuntime, ControlLease,
    ExecutionCapability, ExecutionMode, ExecutionReceipt, LeaseBoundaries, PolicyDecision as RuntimeDecision, ReceiptStatus,
    ScopeAuthorizer, TargetEvidence, VerificationOutcome,
};
use adk_core::{ActingContext, Event, Llm, PolicyDecision, SessionId, SubjectKind, Tool, ToolEffect, ToolOrigin, ToolPolicyRequest, UserId};
use adk_harness::packs::browser::{BROWSER_AUDIT_ACTED, BROWSER_AUDIT_NAVIGATED};
use adk_harness::packs::computer_use::{COMPUTER_USE_AUDIT_EXECUTED, COMPUTER_USE_AUDIT_PREVIEWED};
use adk_harness::{BrowserPack, ComputerUsePack, HarnessDecision, HarnessMode, PromptAnswer, WebConfig};
use anyhow::Result;
use axum::extract::State;
use axum::routing::{get, post};
use axum::Router;
use serde_json::{Value, json};

use crate::{APP, Checks, Env, Outcome, Subject, call, done, outcome, scripted};

pub const SCRIPTED: &[&str] = &["browser_tools_under_modes", "browser_form_fill_after_approval", "computer_use_digest_bound_approval"];

pub const LIVE: &[&str] = &["live_browser_form_fill", "live_computer_use_clipboard"];

const USER: &str = "alice";
const SESSION: &str = "s-packs";
const ACTION_DIGEST: &str = "digest-1";
const POLICY_DIGEST: &str = "policy-1";

// --- The fixture page ----------------------------------------------------------------------

/// A page with a form; every submission is counted and kept.
#[derive(Clone, Default)]
struct Site {
    submissions: Arc<Mutex<Vec<String>>>,
}

async fn page() -> axum::response::Html<&'static str> {
    axum::response::Html(
        "<html><body><h1 id=\"title\">Welcome to the fixture</h1>\
         <form id=\"f\" method=\"post\" action=\"/submit\">\
         <label for=\"name\">Name</label><input id=\"name\" name=\"name\" type=\"text\">\
         <button id=\"submit\" type=\"submit\">Send</button></form></body></html>",
    )
}

async fn submit(State(site): State<Site>, body: String) -> axum::response::Html<String> {
    let name = body
        .split('&')
        .find_map(|pair| pair.strip_prefix("name="))
        .unwrap_or_default()
        .replace('+', " ");
    site.submissions.lock().unwrap().push(name.clone());
    axum::response::Html(format!("<html><body><h1 id=\"title\">Thanks, {name}</h1></body></html>"))
}

async fn serve(site: Site) -> Result<SocketAddr> {
    let app = Router::new().route("/form", get(page)).route("/submit", post(submit)).with_state(site);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Ok(addr)
}

// --- A WebDriver ---------------------------------------------------------------------------

/// A WebDriver endpoint: `ADK_WEBDRIVER_URL`, or a `chromedriver` started from `CHROMEDRIVER`
/// or `PATH`; `Err` names what is missing.
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

async fn driver() -> std::result::Result<Driver, String> {
    if let Ok(url) = std::env::var("ADK_WEBDRIVER_URL")
        && !url.trim().is_empty()
    {
        return Ok(Driver { url: url.trim().to_string(), child: None });
    }
    let binary = std::env::var("CHROMEDRIVER").ok().filter(|b| !b.is_empty()).unwrap_or_else(|| "chromedriver".into());
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|addr| addr.port())
        .map_err(|e| e.to_string())?;
    let mut child = tokio::process::Command::new(&binary)
        .arg(format!("--port={port}"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("no WebDriver ({binary}: {error}); set ADK_WEBDRIVER_URL or CHROMEDRIVER"))?;
    let url = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();
    for _ in 0..50 {
        if client.get(format!("{url}/status")).send().await.is_ok_and(|r| r.status().is_success()) {
            return Ok(Driver { url, child: Some(child) });
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("{binary} exited with {status}"));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(format!("{binary} did not answer /status"))
}

fn session(url: &str) -> Arc<BrowserSession> {
    let mut config = BrowserConfig::default().webdriver_url(url).headless(true);
    for arg in ["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"] {
        config.browser_args.push(arg.to_string());
    }
    Arc::new(BrowserSession::new(config))
}

/// The fixture host is loopback: admitted by the policy for fixtures and by the toolset.
fn fixture_web() -> WebConfig {
    adk_harness::web::fixture::permit_loopback();
    WebConfig { allowed_domains: vec!["127.0.0.1".into()], ..WebConfig::default() }
}

fn baseline_browser_tools(browser: &Arc<BrowserSession>) -> Vec<Arc<dyn Tool>> {
    BrowserToolset::with_profile(browser.clone(), BrowserProfile::FormFilling)
        .with_private_network_access(true)
        .all_tools()
}

fn content_of(response: &Value) -> String {
    response["content"].as_str().unwrap_or_default().to_string()
}

fn response_of(events: &[Event], tool: &str) -> Value {
    crate::responses(events).into_iter().find(|(name, _)| name == tool).map(|(_, response)| response).unwrap_or(Value::Null)
}

async fn approve_all(env: &Env) -> Result<usize> {
    let harness = env.harness.as_ref().expect("harness");
    let (user, session) = (UserId::new(USER)?, SessionId::new(SESSION)?);
    let pending = harness.pending_approvals(&user, &session).await?;
    for approval in &pending {
        harness.decide(&user, &session, &approval.fingerprint, HarnessDecision::Approve).await?;
    }
    Ok(pending.len())
}

async fn resume(env: &Env) -> Result<Vec<Event>> {
    let harness = env.harness.as_ref().expect("harness");
    let stream = harness.resume(&UserId::new(USER)?, &SessionId::new(SESSION)?).await?;
    let results = tokio::time::timeout(Duration::from_secs(120), futures::StreamExt::collect::<Vec<_>>(stream)).await?;
    Ok(results.into_iter().filter_map(|result| result.ok()).collect())
}

fn request(tool: &str, args: Value) -> ToolPolicyRequest {
    ToolPolicyRequest::new(tool, args)
        .with_effect(ToolEffect::NonIdempotent)
        .with_agent_name("harness")
        .with_identity(APP, USER, SESSION)
        .with_invocation_id("inv-1")
        .with_acting_context(ActingContext::derived(APP, "harness", USER, SubjectKind::Human))
}

// --- H10: browser tools under the same modes ------------------------------------------------

async fn browser_tools_under_modes(subject: Subject) -> Result<Checks> {
    let mut checks = Checks::default();
    let script = || {
        scripted(vec![
            call("browser_navigate", json!({ "url": "https://example.com/pricing" })),
            call("browser_type", json!({ "selector": "#name", "text": "Ada" })),
            call("browser_click", json!({ "selector": "#submit" })),
            done(),
        ])
    };
    if subject == Subject::Baseline {
        // The 3.0 agent has no modes: the same calls reach the browser with nobody asked.
        let browser = Arc::new(BrowserSession::new(BrowserConfig::default()));
        let env = Env::in_dir(subject, tempfile::tempdir()?, script(), |b| b, baseline_browser_tools(&browser), true).await?;
        env.session(USER, SESSION).await?;
        let events = env.send(USER, SESSION, "Fill the form on example.com.").await?;
        for tool in ["browser_navigate", "browser_type", "browser_click"] {
            let reached = !matches!(outcome(&events, tool), Outcome::Denied | Outcome::Held);
            checks.check(format!("{tool} reached the browser with nobody asked"), reached && env.prompter.count() == 0);
        }
        return Ok(checks);
    }
    for mode in [HarnessMode::ReadOnly, HarnessMode::Plan] {
        let env = Env::new(subject, script(), |b| b.mode(mode).web(web_example()).pack_browser(BrowserPack::default())).await?;
        env.session(USER, SESSION).await?;
        let events = env.send(USER, SESSION, "Fill the form on example.com.").await?;
        let denied = ["browser_navigate", "browser_type", "browser_click"].iter().all(|tool| outcome(&events, tool) == Outcome::Denied);
        checks.check(format!("in {mode} the navigation, the typing and the click are denied"), denied);
        checks.check(format!("in {mode} nothing is held and the browser never moved"), env.prompter.count() == 0 && env.records(BROWSER_AUDIT_NAVIGATED) == 0);
        let policy = env.harness.as_ref().unwrap().agent().runtime().policy();
        let allowlisted = policy
            .decide(&request("browser_navigate", json!({ "url": "https://docs.rs/crate/adk-core" })), Some(&ToolOrigin::new("static", &json!({}), None)), Some("c"))
            .await;
        checks.check(format!("in {mode} a navigation to an allowlisted host is allowed"), allowlisted == PolicyDecision::Allow);
        let read = policy.decide(&request("browser_extract_text", json!({ "selector": "h1" })), Some(&ToolOrigin::new("static", &json!({}), None)), Some("d")).await;
        checks.check(format!("in {mode} a read of the page is allowed"), read == PolicyDecision::Allow);
    }
    // Ask: the navigation is put to the person with the host shown and the exact-host rule offered.
    let env = Env::new(subject, script(), |b| b.mode(HarnessMode::Ask).web(web_example()).pack_browser(BrowserPack::default())).await?;
    env.session(USER, SESSION).await?;
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    for _ in 0..3 {
        let seen = seen.clone();
        env.prompter.then(move |prompt| {
            seen.lock().unwrap().push(format!(
                "{} | {} | {}",
                prompt.rendered.title,
                prompt.rendered.body.replace('\n', " / "),
                prompt.suggestions.iter().map(|s| s.label.clone()).collect::<Vec<_>>().join(", ")
            ));
            PromptAnswer::Deny { message: Some("not now".into()) }
        });
    }
    let events = env.send(USER, SESSION, "Fill the form on example.com.").await?;
    let seen = seen.lock().unwrap().clone();
    let navigation = seen.iter().find(|line| line.starts_with("browser_navigate"));
    checks.check(
        format!("in ask the navigation prompt shows the host and offers the exact-host rule ({navigation:?})"),
        navigation.is_some_and(|line| line.contains("host: example.com") && line.contains("browser_navigate: any page on example.com")),
    );
    // A human denial on an interactive surface ends the turn (spec 3.1 §9.2 I19): the typing
    // and the click after the denied navigation are not run and nobody is asked again.
    let click = outcome(&events, "browser_click");
    checks.check(
        format!("in ask the denied navigation ends the turn; the click is not run and not asked ({} prompt, click {click:?})", seen.len()),
        seen.len() == 1
            && outcome(&events, "browser_navigate") == Outcome::Denied
            && !matches!(click, Outcome::Executed | Outcome::Held)
            && env.records(BROWSER_AUDIT_ACTED) == 0,
    );
    // Auto: an action is still held; it waits in the inbox.
    let env = Env::in_dir(subject, tempfile::tempdir()?, script(), |b| b.mode(HarnessMode::Auto).web(web_example()).pack_browser(BrowserPack::default()), Vec::new(), false).await?;
    env.session(USER, SESSION).await?;
    let events = env.send(USER, SESSION, "Fill the form on example.com.").await?;
    let pending = env.harness.as_ref().unwrap().pending_approvals(&UserId::new(USER)?, &SessionId::new(SESSION)?).await?;
    checks.check("in auto the navigation to another host is held in the inbox", outcome(&events, "browser_navigate") == Outcome::Held && pending.len() == 1);
    Ok(checks)
}

fn web_example() -> WebConfig {
    WebConfig { allowed_domains: vec!["docs.rs".into()], ..WebConfig::default() }
}

// --- H10: a form filled only after approval -------------------------------------------------

fn form_script_for(url: &str) -> Arc<dyn Llm> {
    scripted(vec![
        call("browser_navigate", json!({ "url": url })),
        call("browser_extract_text", json!({ "selector": "#title" })),
        call("browser_type", json!({ "selector": "#name", "text": "Ada" })),
        call("browser_click", json!({ "selector": "#submit" })),
        call("browser_wait_for_text", json!({ "selector": "#title", "text": "Thanks" })),
        call("browser_extract_text", json!({ "selector": "#title" })),
        done(),
    ])
}

async fn browser_form_fill_after_approval(subject: Subject) -> Result<Checks> {
    let driver = match driver().await {
        Ok(driver) => driver,
        Err(reason) => return Ok(Checks::skipped(&reason)),
    };
    let mut checks = Checks::default();
    let site = Site::default();
    let form = format!("http://{}/form", serve(site.clone()).await?);
    let browser = session(&driver.url);
    if subject == Subject::Baseline {
        let env = Env::in_dir(subject, tempfile::tempdir()?, form_script_for(&form), |b| b, baseline_browser_tools(&browser), true).await?;
        env.session(USER, SESSION).await?;
        let events = env.send(USER, SESSION, "Fill the form.").await?;
        let submitted = site.submissions.lock().unwrap().clone();
        checks.check(format!("the 3.0 agent submitted the form with nobody asked ({submitted:?}, {} prompts)", env.prompter.count()), submitted == ["Ada"] && env.prompter.count() == 0);
        checks.check("the page text reached the 3.0 agent unlabelled", response_of(&events, "browser_extract_text").get("content").is_none());
        browser.stop().await.ok();
        return Ok(checks);
    }
    let env = Env::in_dir(
        subject,
        tempfile::tempdir()?,
        form_script_for(&form),
        |b| b.web(fixture_web()).pack_browser(BrowserPack::new(browser.clone()).with_private_network_access(true)),
        Vec::new(),
        false,
    )
    .await?;
    env.session(USER, SESSION).await?;
    // Run 1: the allowlisted navigation and the read run; the typing is held.
    let events = env.send(USER, SESSION, "Open the form and enter the name Ada.").await?;
    checks.check("the allowlisted navigation ran without asking", outcome(&events, "browser_navigate") == Outcome::Executed);
    let read = response_of(&events, "browser_extract_text");
    let content = content_of(&read);
    checks.check("the page text is delivered inside the untrusted block", content.starts_with("<untrusted-content source=\"browser\"") && content.contains("Welcome to the fixture"));
    checks.check("the typing is held before the browser types", outcome(&events, "browser_type") == Outcome::Held && site.submissions.lock().unwrap().is_empty());
    checks.check("the navigation is audited write-ahead and after the page loaded", env.records(BROWSER_AUDIT_NAVIGATED) == 2);
    // The person approves from the inbox; the typing runs once; the submit is held in turn.
    checks.check("one typing waits in the inbox", approve_all(&env).await? == 1);
    let events = resume(&env).await?;
    checks.check("the approved typing ran and the click is held", outcome(&events, "browser_type") == Outcome::Executed && outcome(&events, "browser_click") == Outcome::Held);
    checks.check("nothing was submitted before the click was approved", site.submissions.lock().unwrap().is_empty());
    checks.check("one click waits in the inbox", approve_all(&env).await? == 1);
    let events = resume(&env).await?;
    let thanks = content_of(&response_of(&events, "browser_extract_text"));
    checks.check(format!("the form was submitted exactly once after approval ({:?})", site.submissions.lock().unwrap()), *site.submissions.lock().unwrap() == ["Ada".to_string()]);
    checks.check("the page thanks Ada inside the untrusted block", thanks.contains("Thanks, Ada"));
    checks.check("both actions are audited as executed", env.records(BROWSER_AUDIT_ACTED) == 2);
    browser.stop().await.ok();
    Ok(checks)
}

// --- H10: a desktop action once, after a digest-bound approval ------------------------------

/// A desktop runtime that previews one clipboard write and counts what it performs.
struct FakeRuntime {
    mutations: AtomicUsize,
    receipts: tokio::sync::Mutex<HashMap<String, ExecutionReceipt>>,
}

impl FakeRuntime {
    fn new() -> Arc<Self> {
        Arc::new(Self { mutations: AtomicUsize::new(0), receipts: tokio::sync::Mutex::new(HashMap::new()) })
    }

    fn mutations(&self) -> usize {
        self.mutations.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl ComputerUseRuntime for FakeRuntime {
    async fn discover_capabilities(&self) -> std::result::Result<Value, ComputerUseError> {
        Ok(json!({ "kind": "capability", "operations": ["write_clipboard"] }))
    }
    async fn observe_visual(&self) -> std::result::Result<Value, ComputerUseError> {
        Ok(json!({ "kind": "visual", "screenshotHash": "sha256:fixture" }))
    }
    async fn observe_semantic(&self) -> std::result::Result<Value, ComputerUseError> {
        Ok(json!({ "kind": "semantic", "windows": [{ "app": "app.reference", "title": "Fixture" }] }))
    }
    async fn preview_action(&self, _: Value) -> std::result::Result<ActionPreview, ComputerUseError> {
        Ok(ActionPreview {
            envelope: ActionEnvelope {
                action_id: "action-1".into(),
                session_id: "session-1".into(),
                execution_group_id: Some("group-1".into()),
                principal_id: "principal-1".into(),
                agent_id: Some("executor".into()),
                tool: "write_clipboard".into(),
                operation: "write_clipboard".into(),
                action_class: ActionClass::EditReversible,
                requested_mode: ExecutionMode::Background,
                target: Some(TargetEvidence {
                    platform: "test".into(),
                    app_id: "app.reference".into(),
                    pid: None,
                    window_id: None,
                    window_title_digest: None,
                    display_id: None,
                    role: None,
                    label_digest: None,
                    bounds: None,
                    observation_id: "observation-1".into(),
                    screenshot_hash: None,
                    ui_tree_revision: None,
                    confidence: 1.0,
                    captured_at: "2026-07-13T10:00:00Z".into(),
                }),
                target_sensitivity: None,
                resource: None,
                provenance: None,
                data_labels: vec!["private".into()],
                postcondition: None,
                reversible: true,
                external_side_effect: false,
                proposed_at: "2026-07-13T10:00:00Z".into(),
                expires_at: "2099-01-01T00:01:00Z".into(),
                args_digest: ACTION_DIGEST.into(),
            },
            executable: false,
            blocker: Some("approval_required".into()),
            policy: RuntimeDecision { decision: "confirm".into(), policy_digest: POLICY_DIGEST.into(), reasons: vec!["clipboard writes need approval".into()], grant_id: None },
            capability: ExecutionCapability {
                app_id: "*".into(),
                operation: "write_clipboard".into(),
                backend: "none".into(),
                supported_modes: vec![ExecutionMode::Shadow, ExecutionMode::Background],
                interference: "none".into(),
                confidence: 1.0,
                verified_at: None,
                verification_source: "platform_rule".into(),
            },
        })
    }
    async fn acquire_lease(&self, envelope: &ActionEnvelope) -> std::result::Result<ControlLease, ComputerUseError> {
        Ok(ControlLease {
            lease_id: "lease-1".into(),
            revision: 1,
            session_id: envelope.session_id.clone(),
            principal_id: envelope.principal_id.clone(),
            agent_id: envelope.agent_id.clone(),
            kind: "cooperative".into(),
            execution_mode: envelope.requested_mode,
            state: "active".into(),
            acquired_at: None,
            expires_at: "2099-01-01T00:01:00Z".into(),
            action_budget: 1,
            actions_used: 0,
            boundaries: LeaseBoundaries::default(),
        })
    }
    async fn execute_action(&self, envelope: &ActionEnvelope, _: &ControlLease, _: Option<&str>) -> std::result::Result<ExecutionReceipt, ComputerUseError> {
        let mut receipts = self.receipts.lock().await;
        if let Some(receipt) = receipts.get(&envelope.action_id) {
            return Ok(receipt.clone());
        }
        self.mutations.fetch_add(1, Ordering::SeqCst);
        let receipt = ExecutionReceipt {
            receipt_id: "receipt-1".into(),
            session_id: envelope.session_id.clone(),
            action_id: envelope.action_id.clone(),
            action_digest: envelope.args_digest.clone(),
            attempt: 1,
            status: ReceiptStatus::Committed,
            created_at: None,
            updated_at: None,
            result: Some(json!({ "ok": true })),
            error: None,
        };
        receipts.insert(envelope.action_id.clone(), receipt.clone());
        Ok(receipt)
    }
    async fn verify(&self, receipt: &ExecutionReceipt, postcondition: Option<&ActionPostcondition>) -> std::result::Result<VerificationOutcome, ComputerUseError> {
        if receipt.status != ReceiptStatus::Committed {
            return Ok(VerificationOutcome::Failed { reason: "not committed".into() });
        }
        Ok(match postcondition {
            Some(_) => VerificationOutcome::Verified,
            None => VerificationOutcome::CommittedUnverified { reason: "no postcondition declared".into() },
        })
    }
}

fn desktop_pack(runtime: &Arc<FakeRuntime>) -> ComputerUsePack {
    ComputerUsePack::new(runtime.clone(), ScopeAuthorizer::new(["computer:plan", "computer:execute:background"]))
}

fn summary() -> Value {
    json!({
        "tool": "write_clipboard",
        "operation": "write_clipboard",
        "actionClass": "edit_reversible",
        "requestedMode": "background",
        "targetApp": "app.reference",
        "reversible": true,
        "externalSideEffect": false,
    })
}

fn execute_args(action_digest: &str, action: Value) -> Value {
    json!({ "actionDigest": action_digest, "policyDigest": POLICY_DIGEST, "action": action })
}

fn proposed() -> Value {
    json!({ "action": { "tool": "write_clipboard", "operation": "write_clipboard", "text": "hello" } })
}

async fn computer_use_digest_bound_approval(subject: Subject) -> Result<Checks> {
    if subject == Subject::Baseline {
        return Ok(Checks::skipped("the 3.0 CodingAgent has no governed desktop tools"));
    }
    let mut checks = Checks::default();
    // The honest flow: observe, preview, execute held, approved from the inbox, one mutation.
    let runtime = FakeRuntime::new();
    let model = scripted(vec![
        call("computer_use_observe", json!({})),
        call("computer_use_preview", proposed()),
        call("computer_use_execute", execute_args(ACTION_DIGEST, summary())),
        done(),
    ]);
    let env = Env::in_dir(subject, tempfile::tempdir()?, model, |b| b.pack_computer_use(desktop_pack(&runtime)), Vec::new(), false).await?;
    env.session(USER, SESSION).await?;
    let events = env.send(USER, SESSION, "Write hello to the clipboard.").await?;
    let preview = response_of(&events, "computer_use_preview");
    checks.check("the preview names the digests and the action", preview["actionDigest"] == ACTION_DIGEST && preview["route"] == "approval" && preview["action"] == summary());
    checks.check("the execution is held and nothing moved", outcome(&events, "computer_use_execute") == Outcome::Held && runtime.mutations() == 0);
    checks.check("one desktop action waits in the inbox", approve_all(&env).await? == 1);
    let events = resume(&env).await?;
    let executed = response_of(&events, "computer_use_execute");
    checks.check(format!("the action ran once with a receipt ({executed})"), outcome(&events, "computer_use_execute") == Outcome::Executed && executed["receiptId"] == "receipt-1" && runtime.mutations() == 1);
    checks.check("the preview and the execution are audited", env.records(COMPUTER_USE_AUDIT_PREVIEWED) == 1 && env.records(COMPUTER_USE_AUDIT_EXECUTED) == 1);
    // A changed digest, approved, runs nothing.
    let runtime = FakeRuntime::new();
    let model = scripted(vec![call("computer_use_preview", proposed()), call("computer_use_execute", execute_args("digest-other", summary())), done()]);
    let env = Env::new(subject, model, |b| b.pack_computer_use(desktop_pack(&runtime))).await?;
    env.session(USER, SESSION).await?;
    let events = env.send(USER, SESSION, "Write hello to the clipboard.").await?;
    let error = response_of(&events, "computer_use_execute")["error"].as_str().unwrap_or_default().to_string();
    checks.check(format!("a changed digest is refused after approval ({error})"), error.contains("preview_mismatch") && env.prompter.count() == 1 && runtime.mutations() == 0);
    // A misdescribed action, approved, runs nothing.
    let runtime = FakeRuntime::new();
    let mut lie = summary();
    lie["operation"] = json!("read_clipboard");
    let model = scripted(vec![call("computer_use_preview", proposed()), call("computer_use_execute", execute_args(ACTION_DIGEST, lie)), done()]);
    let env = Env::new(subject, model, |b| b.pack_computer_use(desktop_pack(&runtime))).await?;
    env.session(USER, SESSION).await?;
    let events = env.send(USER, SESSION, "Write hello to the clipboard.").await?;
    let error = response_of(&events, "computer_use_execute")["error"].as_str().unwrap_or_default().to_string();
    checks.check(format!("a misdescribed action is refused after approval ({error})"), error.contains("differs from the preview") && runtime.mutations() == 0);
    // Plan denies the action outright.
    let runtime = FakeRuntime::new();
    let model = scripted(vec![call("computer_use_execute", execute_args(ACTION_DIGEST, summary())), done()]);
    let env = Env::new(subject, model, |b| b.mode(HarnessMode::Plan).pack_computer_use(desktop_pack(&runtime))).await?;
    env.session(USER, SESSION).await?;
    let events = env.send(USER, SESSION, "Write hello to the clipboard.").await?;
    checks.check("in plan the desktop action is denied", outcome(&events, "computer_use_execute") == Outcome::Denied && runtime.mutations() == 0);
    Ok(checks)
}

pub async fn run_scripted(name: &str, subject: Subject) -> Option<Result<Checks>> {
    Some(match name {
        "browser_tools_under_modes" => browser_tools_under_modes(subject).await,
        "browser_form_fill_after_approval" => browser_form_fill_after_approval(subject).await,
        "computer_use_digest_bound_approval" => computer_use_digest_bound_approval(subject).await,
        _ => return None,
    })
}

// --- Live ------------------------------------------------------------------------------------

async fn live_browser_form_fill(model: Arc<dyn Llm>) -> Result<Checks> {
    let driver = match driver().await {
        Ok(driver) => driver,
        Err(reason) => return Ok(Checks::skipped(&reason)),
    };
    let mut checks = Checks::default();
    let site = Site::default();
    let form = format!("http://{}/form", serve(site.clone()).await?);
    let browser = session(&driver.url);
    let env = Env::new(Subject::Harness, model, |b| {
        b.web(fixture_web()).pack_browser(BrowserPack::new(browser.clone()).with_private_network_access(true))
    })
    .await?;
    env.session(USER, SESSION).await?;
    let events = env
        .send(
            USER,
            SESSION,
            &format!(
                "Using the browser tools: open {form}, read the heading with browser_extract_text on the selector #title, \
                 type the name Ada into the input with selector #name, click the button with selector #submit, then read \
                 the heading again and tell me what it says. Do not ask me for permission in your reply; the tools ask."
            ),
        )
        .await?;
    let seen = env.prompter.seen.lock().unwrap().clone();
    let executed_actions = crate::responses(&events)
        .iter()
        .filter(|(name, response)| matches!(name.as_str(), "browser_type" | "browser_click" | "browser_press_key") && !adk_core::is_confirmation_pending(response) && response.get("error").is_none())
        .count();
    let prompted_actions = seen.iter().filter(|line| line.starts_with("browser_type") || line.starts_with("browser_click") || line.starts_with("browser_press_key")).count();
    checks.check(format!("every executed action was approved first ({prompted_actions} prompts, {executed_actions} executed)"), executed_actions >= 1 && prompted_actions >= executed_actions);
    checks.check("the allowlisted navigation needed no prompt", !seen.iter().any(|line| line.starts_with("browser_navigate")));
    let submissions = site.submissions.lock().unwrap().clone();
    checks.check(format!("the form was submitted exactly once with Ada ({submissions:?})"), submissions == ["Ada"]);
    let reads = crate::responses(&events).into_iter().filter(|(name, _)| name == "browser_extract_text").map(|(_, response)| content_of(&response)).collect::<Vec<_>>();
    checks.check("the page text reached the model inside the untrusted block", reads.iter().any(|content| content.starts_with("<untrusted-content source=\"browser\"") && content.contains("Welcome")));
    checks.check("the navigation is audited", env.records(BROWSER_AUDIT_NAVIGATED) >= 2);
    browser.stop().await.ok();
    Ok(checks)
}

async fn live_computer_use_clipboard(model: Arc<dyn Llm>) -> Result<Checks> {
    let mut checks = Checks::default();
    let runtime = FakeRuntime::new();
    let env = Env::new(Subject::Harness, model, |b| b.pack_computer_use(desktop_pack(&runtime))).await?;
    env.session(USER, SESSION).await?;
    let events = env
        .send(
            USER,
            SESSION,
            "Using the computer-use tools, write the text \"hello\" to the clipboard: call computer_use_preview with \
             action {\"tool\": \"write_clipboard\", \"operation\": \"write_clipboard\", \"text\": \"hello\"}, then call \
             computer_use_execute with the preview's actionDigest, policyDigest and action exactly as returned. Do not \
             ask me for permission in your reply; the tools ask. Then report the receipt id.",
        )
        .await?;
    let executed = response_of(&events, "computer_use_execute");
    checks.check(format!("the action ran exactly once with a receipt ({} mutations, {executed})", runtime.mutations()), runtime.mutations() == 1 && executed["receiptId"] == "receipt-1");
    let seen = env.prompter.seen.lock().unwrap().clone();
    checks.check(format!("the execution was approved once at the prompt ({} prompts)", seen.len()), seen.iter().filter(|line| line.starts_with("computer_use_execute")).count() == 1);
    checks.check("the preview and the execution are audited", env.records(COMPUTER_USE_AUDIT_PREVIEWED) >= 1 && env.records(COMPUTER_USE_AUDIT_EXECUTED) == 1);
    Ok(checks)
}

pub async fn run_live(name: &str, model: Arc<dyn Llm>) -> Option<Result<Checks>> {
    Some(match name {
        "live_browser_form_fill" => live_browser_form_fill(model).await,
        "live_computer_use_clipboard" => live_computer_use_clipboard(model).await,
        _ => return None,
    })
}
