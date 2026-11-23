//! S14, the Screen copilot: copy customer C-1042 from a web CRM into a desktop form on a
//! display at device pixel ratio 2, through a harness in Ask mode with the browser and
//! computer-use packs.
//!
//! The browser half reads the CRM through the governed browser tools: `browser_elements`
//! gives set-of-marks candidates at the page's ratio, the chosen mark becomes a
//! `browser_click` by selector, and `browser_extract_text` reads the record. The desktop half
//! acts only through `computer_use_preview` and the held `computer_use_execute`, whose
//! reference graph drives `ScreenRuntime`: PP-OCRv6 marks on each screenshot, coordinates in
//! the fitted screenshot's pixels grounded through `ScreenSpace`, one `ScreenPlan` per window
//! checked again before every step, and the verifier after every step. The form has two
//! identical "Save" buttons; the copilot grounds "Save" by label and reading order and picks
//! the notes' one, the verifier sees no "Customer saved", and the copilot grounds it again.
//!
//! Stand-ins: the WebDriver server and the desktop (`crm.rs`, `screen.rs`), and the planner,
//! which names marks by their labels. The CRM, the browser tools, the harness and its
//! packs, the governed graph, OCR, marks, plans, fitting and the verifier are the real ones.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use adk_browser::{BrowserConfig, BrowserSession};
use adk_computer_use::ScopeAuthorizer;
use adk_core::audit_log::{AuditRecordDraft, InMemoryAuditRecorder};
use adk_core::{
    Content, FinishReason, Llm, LlmRequest, LlmResponse, LlmResponseStream, Part, SessionId,
    UserId,
};
use adk_harness::packs::browser::{BROWSER_AUDIT_ACTED, BROWSER_AUDIT_NAVIGATED, BrowserPack};
use adk_harness::packs::computer_use::{
    COMPUTER_USE_AUDIT_EXECUTED, COMPUTER_USE_EXECUTE, ComputerUsePack,
};
use adk_harness::{
    Harness, HarnessAgent, HarnessMode, InstructionsConfig, WebConfig, Workspace,
};
use adk_vision::screen::{BrowserElements, GroundedTarget, MarkSet, ScreenshotScale};
use adk_vision::{BBox, DecodeLimits, Digest, ScreenSpace, SourceImage};
use anyhow::{Context, Result};
use base64::Engine;
use futures::StreamExt;
use serde_json::{Value, json};

use crate::Checks;
use crate::crm::{Crm, Customer, StandInBrowser};
use crate::fixtures::{Approver, models, tool_responses};
use crate::screen::{Desktop, ScreenEvent, ScreenRuntime, norm, strings};

const APP: &str = "screen-copilot";
const USER: &str = "ana";
const SESSION: &str = "copy-c-1042";
const CUSTOMER: &str = "C-1042";
const FORM: &str = "Customer form";
const BROWSER: &str = "Larch CRM - Chromium";
const MAIL: &str = "Mail";

fn customers(record: &BTreeMap<String, String>) -> Vec<Customer> {
    let first = ["Name", "Email", "Phone", "Company", "Account"]
        .iter()
        .map(|label| {
            let value = record.get(&label.to_lowercase()).cloned().unwrap_or_default();
            (label.to_string(), value)
        })
        .collect();
    let other = |id: &str, name: &str, email: &str, phone: &str, company: &str, account: &str| {
        Customer {
            id: id.into(),
            fields: [
                ("Name", name),
                ("Email", email),
                ("Phone", phone),
                ("Company", company),
                ("Account", account),
            ]
            .iter()
            .map(|(l, v)| (l.to_string(), v.to_string()))
            .collect(),
        }
    };
    vec![
        Customer { id: CUSTOMER.into(), fields: first },
        other(
            "C-1043",
            "Bruno Varga",
            "bruno.varga@example.com",
            "+44 20 7946 0123",
            "Harbour Hardware",
            "ACC-104311",
        ),
        other(
            "C-1044",
            "Chen Lindqvist",
            "chen.lindqvist@example.com",
            "+44 20 7946 0412",
            "Copperleaf Books",
            "ACC-104377",
        ),
    ]
}

/// The last tool answer in the request: the tool's name and what it returned.
fn last_answer(request: &LlmRequest) -> Option<(String, Value)> {
    request.contents.iter().rev().find_map(|content| {
        content.parts.iter().rev().find_map(|part| match part {
            Part::FunctionResponse { function_response, .. } => {
                Some((function_response.name.clone(), function_response.response.clone()))
            }
            _ => None,
        })
    })
}

fn user_text(request: &LlmRequest) -> String {
    request
        .contents
        .iter()
        .filter(|content| content.role == "user")
        .flat_map(|content| content.parts.iter().filter_map(Part::text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The JSON a governed browser read delivered inside its untrusted block.
fn untrusted_json(answer: &Value) -> Option<Value> {
    let content = answer.get("content")?.as_str()?;
    let start = content.find('\n')? + 1;
    let end = content.rfind("\n</untrusted-content>")?;
    serde_json::from_str(content.get(start..end)?).ok()
}

/// One desktop step the copilot plans: the action it previews, then executes.
#[derive(Clone, Debug)]
struct Step {
    action: Value,
    preview: Option<Value>,
    result: Option<Value>,
}

#[derive(Default)]
struct Plan {
    calls: usize,
    page: Option<(Digest, (u32, u32))>,
    /// The CRM mark chosen and the target it grounded to.
    link: Option<GroundedTarget>,
    labels: Vec<String>,
    record: Vec<(String, String)>,
    steps: Vec<Step>,
    next: usize,
    /// The scale between the screenshot and the fitted copy the copilot sees.
    scale: Option<ScreenshotScale>,
    /// Where the first Save was clicked, in screenshot pixels, once it failed.
    failed_save: Option<[f32; 2]>,
    done: bool,
}

/// The copilot's model, a stand-in that plans from the tool answers it is shown: it names
/// each mark by its label and grounds "Save" by label and reading order, as a model that
/// cannot tell two identical buttons apart would.
#[derive(Default)]
struct Copilot {
    plan: Mutex<Plan>,
}

fn reply(part: Part) -> LlmResponseStream {
    let mut response = LlmResponse::new(Content { role: "model".into(), parts: vec![part] });
    response.finish_reason = Some(FinishReason::Stop);
    response.turn_complete = true;
    Box::pin(futures::stream::iter([Ok(response)]))
}

fn text(text: impl Into<String>) -> LlmResponseStream {
    reply(Part::Text { text: text.into() })
}

/// The marks of `window` in an observation, in reading order: label and box.
fn marks_of(visual: &Value, window: &str) -> Vec<(String, BBox)> {
    visual["marks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["window"] == window)
        .filter_map(|m| {
            let b: Vec<f32> =
                m["box"].as_array()?.iter().filter_map(|v| v.as_f64().map(|v| v as f32)).collect();
            Some((m["label"].as_str()?.to_string(), BBox::new(b[0], b[1], b[2], b[3])))
        })
        .collect()
}

/// The scale between an observation's screenshot and the fitted copy the copilot sees.
fn scale_of(visual: &Value) -> Option<ScreenshotScale> {
    let pair = |v: &Value| Some((v[0].as_u64()? as u32, v[1].as_u64()? as u32));
    Some(ScreenshotScale::new(pair(&visual["size"])?, pair(&visual["sent"])?))
}

/// A point of the full screenshot as the whole pixel of the fitted copy the copilot sees.
fn sent_pixel(visual: &Value, point: [f32; 2]) -> Option<[i64; 2]> {
    let sent = scale_of(visual)?.to_sent(point);
    Some([sent[0].floor() as i64, sent[1].floor() as i64])
}

impl Copilot {
    fn call(&self, plan: &mut Plan, name: &str, args: Value) -> LlmResponseStream {
        plan.calls += 1;
        reply(Part::FunctionCall {
            name: name.into(),
            args,
            id: Some(format!("{name}-{}", plan.calls)),
            thought_signature: None,
        })
    }

    /// The form's steps on one observation: click each field's input (the mark to the
    /// right of its label), type the CRM's value, then click "Save"; and one step aimed at
    /// Mail's "Send", which the runtime must refuse.
    fn plan_desktop(plan: &mut Plan, visual: &Value) -> Option<()> {
        let screenshot = visual["screenshot"].as_str()?.to_string();
        let form = marks_of(visual, FORM);
        let action = |step: String, operation: &str, extra: Value, window: &str| {
            let mut action = json!({
                "tool": "computer",
                "operation": operation,
                "screenshot": screenshot,
                "window": window,
                "step": step,
            });
            if let (Value::Object(a), Value::Object(e)) = (&mut action, extra) {
                a.extend(e);
            }
            Step { action, preview: None, result: None }
        };
        let mut n = 0;
        for (label, value) in plan.record.clone() {
            let (_, at) = form.iter().find(|(l, _)| l.eq_ignore_ascii_case(&label))?;
            let (placeholder, input) = form
                .iter()
                .filter(|(_, b)| (b.center()[1] - at.center()[1]).abs() < 30.0 && b.x0 > at.x1)
                .min_by(|a, b| a.1.x0.total_cmp(&b.1.x0))?;
            n += 1;
            let coordinate = sent_pixel(visual, input.center())?;
            plan.steps.push(action(
                format!("c{n}"),
                "left_click",
                json!({ "coordinate": coordinate, "expect": {} }),
                FORM,
            ));
            n += 1;
            plan.steps.push(action(
                format!("c{n}"),
                "type",
                json!({
                    "text": value,
                    "expect": { "appear": [value], "disappear": [placeholder] },
                }),
                FORM,
            ));
        }
        // Two "Save" marks look the same; the first in reading order is the notes' one.
        let (_, save) = form.iter().find(|(l, _)| l == "Save")?;
        n += 1;
        plan.steps.push(action(
            format!("c{n}"),
            "left_click",
            json!({
                "coordinate": sent_pixel(visual, save.center())?,
                "expect": { "appear": ["Customer saved"] },
            }),
            FORM,
        ));
        let (_, send) = marks_of(visual, MAIL).into_iter().find(|(l, _)| l == "Send")?;
        plan.steps.push(action(
            "m1".into(),
            "left_click",
            json!({ "coordinate": sent_pixel(visual, send.center())?, "expect": {} }),
            MAIL,
        ));
        Some(())
    }

    /// After the verifier failed the first Save: the other "Save" on a fresh observation.
    fn reground_save(plan: &mut Plan, visual: &Value) -> Option<()> {
        let tried = plan.failed_save?;
        let (_, save) = marks_of(visual, FORM)
            .into_iter()
            .find(|(l, b)| l == "Save" && !b.contains(tried))?;
        plan.steps.push(Step {
            action: json!({
                "tool": "computer",
                "operation": "left_click",
                "screenshot": visual["screenshot"],
                "window": FORM,
                "step": "c12",
                "coordinate": sent_pixel(visual, save.center())?,
                "expect": { "appear": ["Customer saved"] },
            }),
            preview: None,
            result: None,
        });
        Some(())
    }

    /// The next call: preview every planned step not yet previewed, then execute every
    /// approved one in order.
    fn next_desktop(&self, plan: &mut Plan) -> LlmResponseStream {
        if let Some(i) = plan.steps.iter().position(|s| s.preview.is_none()) {
            let action = plan.steps[i].action.clone();
            plan.next = i;
            return self.call(plan, "computer_use_preview", json!({ "action": action }));
        }
        if let Some(i) = plan
            .steps
            .iter()
            .position(|s| s.result.is_none() && s.preview.as_ref().is_some_and(|p| p["route"] == "approval"))
        {
            let preview = plan.steps[i].preview.clone().unwrap_or_default();
            plan.next = i;
            return self.call(
                plan,
                COMPUTER_USE_EXECUTE,
                json!({
                    "actionDigest": preview["actionDigest"],
                    "policyDigest": preview["policyDigest"],
                    "action": preview["action"],
                }),
            );
        }
        plan.done = true;
        text(format!("Copied {CUSTOMER} into the customer form and saved it."))
    }
}

#[async_trait::async_trait]
impl Llm for Copilot {
    fn name(&self) -> &str {
        "copilot-planner"
    }

    async fn generate_content(
        &self,
        request: LlmRequest,
        _stream: bool,
    ) -> adk_core::Result<LlmResponseStream> {
        let mut guard = self.plan.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let plan = &mut *guard;
        if plan.done {
            return Ok(text("Done."));
        }
        let Some((tool, answer)) = last_answer(&request) else {
            let message = user_text(&request);
            let Some(url) = message.split_whitespace().find(|w| w.starts_with("http://")) else {
                return Ok(text("Which CRM page?"));
            };
            let url = url.trim_end_matches(['.', ',']).to_string();
            return Ok(self.call(plan, "browser_navigate", json!({ "url": url })));
        };
        if answer.get("error").is_some() {
            plan.done = true;
            return Ok(text(format!("{tool} failed: {}", answer["error"])));
        }
        Ok(match tool.as_str() {
            "browser_navigate" => self.call(plan, "browser_screenshot", json!({})),
            "browser_screenshot" => {
                let png = answer["base64_image"]
                    .as_str()
                    .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
                    .unwrap_or_default();
                if let Ok(image) = SourceImage::decode(&png, &DecodeLimits::default()) {
                    plan.page = Some((image.digest(), image.size()));
                }
                self.call(plan, "browser_elements", json!({}))
            }
            "browser_elements" => {
                let page: Option<BrowserElements> =
                    untrusted_json(&answer).and_then(|v| serde_json::from_value(v).ok());
                let (Some(page), Some((digest, size))) = (page, plan.page) else {
                    plan.done = true;
                    return Ok(text("The CRM page gave no elements."));
                };
                let screen = ScreenSpace::new("viewport", [0.0, 0.0], page.device_pixel_ratio);
                let mut marks = MarkSet::new(digest, size, screen);
                marks.add_browser_elements(&page);
                let chosen = marks
                    .candidates
                    .iter()
                    .find(|c| c.label.as_deref().is_some_and(|l| l.contains(CUSTOMER)))
                    .map(|c| c.mark);
                let Some(target) = chosen.and_then(|mark| marks.ground(mark).ok()) else {
                    plan.done = true;
                    return Ok(text(format!("No mark names {CUSTOMER}.")));
                };
                let args = target.browser_click_args().unwrap_or_default();
                plan.link = Some(target);
                self.call(plan, "browser_click", args)
            }
            "browser_click" => self.call(
                plan,
                "browser_extract_text",
                json!({ "selector": "#record dt", "all": true }),
            ),
            "browser_extract_text" if plan.labels.is_empty() => {
                plan.labels = untrusted_json(&answer)
                    .map(|v| strings(&v["texts"]))
                    .unwrap_or_default();
                self.call(
                    plan,
                    "browser_extract_text",
                    json!({ "selector": "#record dd", "all": true }),
                )
            }
            "browser_extract_text" => {
                let values = untrusted_json(&answer)
                    .map(|v| strings(&v["texts"]))
                    .unwrap_or_default();
                plan.record = plan.labels.iter().cloned().zip(values).collect();
                self.call(plan, "computer_use_observe", json!({}))
            }
            "computer_use_observe" => {
                let visual = &answer["visual"];
                plan.scale = scale_of(visual);
                let planned = if plan.steps.is_empty() {
                    Self::plan_desktop(plan, visual)
                } else {
                    Self::reground_save(plan, visual)
                };
                if planned.is_none() {
                    plan.done = true;
                    return Ok(text("The form's fields are not on the screen."));
                }
                self.next_desktop(plan)
            }
            "computer_use_preview" => {
                let i = plan.next;
                plan.steps[i].preview = Some(answer);
                self.next_desktop(plan)
            }
            COMPUTER_USE_EXECUTE => {
                let i = plan.next;
                let verified = answer["verified"] == true;
                plan.steps[i].result = Some(answer);
                if !verified {
                    let action = &plan.steps[i].action;
                    let is_save = action["expect"]["appear"][0] == "Customer saved";
                    if is_save && plan.failed_save.is_none() {
                        // The verifier saw no "Customer saved": ground "Save" again, once.
                        let at = action["coordinate"].as_array().and_then(|c| {
                            let point = [c.first()?.as_f64()? as f32, c.get(1)?.as_f64()? as f32];
                            Some(plan.scale?.to_source([point[0] + 0.5, point[1] + 0.5]))
                        });
                        plan.failed_save = at;
                        return Ok(self.call(plan, "computer_use_observe", json!({})));
                    }
                    plan.done = true;
                    return Ok(text("A step failed its check twice; a person decides."));
                }
                self.next_desktop(plan)
            }
            other => {
                plan.done = true;
                text(format!("Unexpected answer from {other}."))
            }
        })
    }
}

fn records(audit: &InMemoryAuditRecorder, action: &str) -> Vec<AuditRecordDraft> {
    audit.records().into_iter().filter(|r| r.action == action).collect()
}

pub async fn run() -> Result<Checks> {
    let mut checks = Checks::default();
    checks.note(
        "a harness in Ask mode with the browser and computer-use packs; PP-OCRv6 tiny marks on \
         the device; a stand-in WebDriver, desktop and planner",
    );
    adk_harness::web::fixture::permit_loopback();

    let desktop = Arc::new(Desktop::load()?);
    let crm = Crm::serve(customers(&desktop.layout.record)).await?;
    let browser = StandInBrowser::start().await?;
    let session = Arc::new(BrowserSession::new(
        BrowserConfig::default().webdriver_url(browser.url()).headless(true),
    ));
    let reader = models().await?.reader.clone();
    let runtime = Arc::new(ScreenRuntime::new(
        desktop.clone(),
        reader,
        vec![BROWSER.to_string(), FORM.to_string()],
    )?);

    let dir = tempfile::tempdir()?;
    let copilot = Arc::new(Copilot::default());
    let approver = Arc::new(Approver::default());
    let audit = InMemoryAuditRecorder::new();
    let agent = Arc::new(
        HarnessAgent::builder("copilot")
            .model(copilot.clone())
            .workspace(Workspace::new(dir.path()))
            .project_instructions(InstructionsConfig {
                user_file: None,
                require_trust: false,
                ..Default::default()
            })
            .mode(HarnessMode::Ask)
            .prompter(approver.clone())
            .audit_log(Arc::new(audit.clone()))
            .web(WebConfig { allowed_domains: vec!["127.0.0.1".into()], ..WebConfig::default() })
            .pack_browser(BrowserPack::new(session.clone()).with_private_network_access(true))
            .pack_computer_use(ComputerUsePack::new(
                runtime.clone(),
                ScopeAuthorizer::new(["computer:plan", "computer:execute:background"]),
            ))
            .without_compaction()
            .build()?,
    );
    let sessions: Arc<dyn adk_session::SessionService> =
        Arc::new(adk_session::InMemorySessionService::new());
    let runner = agent
        .configure_runner(
            adk_runner::Runner::builder().app_name(APP).agent(agent.clone()).session_service(sessions),
        )
        .audit_log(Arc::new(audit.clone()))
        .build()?;
    let harness = Harness::with_runner(agent, runner)?;
    let (user, session_id) = (UserId::new(USER)?, SessionId::new(SESSION)?);
    harness.open_session(&user, Some(&session_id)).await?;
    let ask = format!(
        "Copy customer {CUSTOMER} from the CRM at {} into the Customer form on the desktop, then save it.",
        crm.url("/customers")
    );
    let mut stream = harness.send(&user, &session_id, &ask).await?;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event?);
    }
    session.stop().await.ok();

    let plan = copilot.plan.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let screen_events = runtime.events();
    let actions = desktop.actions();
    let layout = &desktop.layout;
    let screen = layout.screen();
    let crm_record = crm.customer(CUSTOMER).context("the CRM holds C-1042")?.clone();

    // The browser half: the record read through the governed browser tools.
    let read: BTreeMap<String, String> = plan.record.iter().cloned().collect();
    let expected: BTreeMap<String, String> = crm_record.fields.iter().cloned().collect();
    checks.check(
        &format!(
            "the copilot read {CUSTOMER} from the CRM through governed browser calls: the record's {} fields as the CRM holds them",
            expected.len()
        ),
        !read.is_empty() && read == expected,
    );

    // Every click lands inside its target.
    let link = plan.link.clone().context("the copilot chose a CRM mark")?;
    let clicks = browser.clicks();
    let link_inside = clicks.len() == 1
        && link.browser_click_args() == Some(json!({ "selector": clicks[0].selector }))
        && {
            let [x, y, w, h] = clicks[0].rect;
            link.points.contains(link.click)
                && BBox::from_xywh(x, y, w, h).contains(link.click)
        };
    let mut desktop_clicks = 0;
    let mut inside = 0;
    for event in &screen_events {
        let ScreenEvent::Executed { target: Some(target), actions: range, .. } = event else {
            continue;
        };
        for action in &actions[range.clone()] {
            let Some(pixel) = action.pixel else { continue };
            desktop_clicks += 1;
            if target.pixels.contains(pixel) && target.points.contains(screen.to_points(pixel)) {
                inside += 1;
            }
        }
    }
    checks.check(
        &format!(
            "every click lands inside its target: the CRM link by selector inside its element at device pixel ratio {}, and {inside}/{desktop_clicks} desktop clicks, given in the fitted 2380×1546 screenshot's pixels, inside their targets in pixels and in points",
            link.screen.device_pixel_ratio
        ),
        link_inside && desktop_clicks >= 7 && inside == desktop_clicks,
    );

    // Every action is a governed browser or computer_use_execute call.
    let executed: Vec<&str> = screen_events
        .iter()
        .filter_map(|e| match e {
            ScreenEvent::Executed { action_id, deviation: None, .. } => Some(action_id.as_str()),
            _ => None,
        })
        .collect();
    let executed_records = records(&audit, COMPUTER_USE_AUDIT_EXECUTED);
    let receipts: Vec<String> = executed_records
        .iter()
        .filter_map(|r| r.ext.get("receiptId").and_then(Value::as_str).map(str::to_string))
        .collect();
    let desktop_governed = !actions.is_empty()
        && actions.iter().all(|a| {
            a.governed_by.as_deref().is_some_and(|id| {
                executed.contains(&id) && receipts.contains(&format!("receipt-{id}"))
            })
        })
        && actions.len() == executed.len();
    let log = browser.log();
    let navigations = log.iter().filter(|l| l.as_str() == "POST /url").count();
    let browser_clicks = log.iter().filter(|l| l.starts_with("POST /element/") && l.ends_with("/click")).count();
    let navigated = records(&audit, BROWSER_AUDIT_NAVIGATED)
        .iter()
        .filter(|r| r.ext.get("phase").and_then(Value::as_str) == Some("response"))
        .count();
    let acted = records(&audit, BROWSER_AUDIT_ACTED)
        .iter()
        .filter(|r| r.ext.get("tool").and_then(Value::as_str) == Some("browser_click"))
        .count();
    let prompts = approver.prompts();
    let approved = |tool: &str| prompts.iter().filter(|p| p.request.tool_name == tool).count();
    checks.check(
        &format!(
            "every action is a governed browser or computer_use_execute call: {} desktop actions, each inside one of {} approved computer_use_execute calls; {navigations} navigation and {browser_clicks} click in the browser, each a governed browser call ({} approved)",
            actions.len(),
            approved(COMPUTER_USE_EXECUTE),
            approved("browser_click")
        ),
        desktop_governed
            && approved(COMPUTER_USE_EXECUTE) == executed.len()
            && navigations == 1
            && navigated == navigations
            && browser_clicks == 1
            && acted == browser_clicks
            && approved("browser_click") == browser_clicks,
    );

    // The seeded mis-grounding: caught by the verifier and grounded again.
    let widget_of = |step: &str| -> Vec<Option<String>> {
        screen_events
            .iter()
            .filter_map(|e| match e {
                ScreenEvent::Executed { step: s, actions: range, .. } if s == step => {
                    Some(actions[range.clone()].iter().map(|a| a.widget.clone()).collect::<Vec<_>>())
                }
                _ => None,
            })
            .flatten()
            .collect()
    };
    let report = |step: &str| {
        screen_events.iter().find_map(|e| match e {
            ScreenEvent::Verified { step: s, report, .. } if s == step => Some(report.clone()),
            _ => None,
        })
    };
    let first = report("c11");
    let again = report("c12");
    let (saved, saves) = desktop.saved();
    let caught = first.as_ref().is_some_and(|r| {
        !r.passed && r.missing.iter().any(|m| m.contains(&norm("Customer saved")))
    }) && widget_of("c11") == vec![Some("note-save".to_string())];
    let regrounded = again.as_ref().is_some_and(|r| r.passed)
        && widget_of("c12") == vec![Some("save".to_string())]
        && saves == 1;
    checks.check(
        "the seeded mis-grounding is caught by the verifier and grounded again: the notes' Save, identical to the footer's, left no \"Customer saved\"; grounded again on the footer's Save, the form saved once",
        caught && regrounded,
    );
    if let Some(r) = &first {
        checks.note(format!("verifier on the first Save: {}", r.missing.join("; ")));
    }

    // No action outside the two allowed windows.
    let crm_origin = crm.url("/");
    let probe = screen_events.iter().find_map(|e| match e {
        ScreenEvent::Previewed { step, route, reason, .. } if step == "m1" => {
            Some((*route, reason.clone().unwrap_or_default()))
        }
        _ => None,
    });
    let probe_ran = screen_events
        .iter()
        .any(|e| matches!(e, ScreenEvent::Executed { step, .. } if step == "m1"));
    let in_allowed = actions
        .iter()
        .all(|a| matches!(a.window.as_deref(), Some(FORM) | Some(BROWSER)));
    let loads = browser.loads();
    checks.check(
        &format!(
            "no action outside the two allowed windows: the desktop's {} actions were all in {FORM}, the browser loaded only the CRM's {} pages, and the step aimed at Mail's Send was blocked at preview and never ran",
            actions.len(),
            loads.len()
        ),
        in_allowed
            && !actions.iter().any(|a| a.window.as_deref() == Some(MAIL))
            && !loads.is_empty()
            && loads.iter().all(|u| u.starts_with(&crm_origin))
            && probe.as_ref().is_some_and(|(route, reason)| {
                *route == "blocked" && reason.contains("not one of the windows")
            })
            && !probe_ran,
    );
    if let Some((_, reason)) = &probe {
        checks.note(format!("Mail's Send: {reason}"));
    }

    // Targets bound to their screenshot (V-I14), one plan in one window, every step checked.
    let pointing: Vec<_> = screen_events
        .iter()
        .filter_map(|e| match e {
            ScreenEvent::Previewed { route: "approval", target: Some(_), evidence_bound, .. } => {
                Some(*evidence_bound)
            }
            _ => None,
        })
        .collect();
    let plans = runtime.plans();
    let form_plan = plans.iter().find(|p| p.steps.len() == 11);
    let checked = screen_events
        .iter()
        .filter(|e| matches!(e, ScreenEvent::Executed { deviation: None, .. }))
        .count();
    checks.check(
        &format!(
            "every desktop target's evidence binds its screenshot's digest and states logical points at ratio 2 ({} targets); the 11 form steps are one plan in {FORM}, and every one of the {checked} steps run was grounded again on a fresh screenshot and matched its preview",
            pointing.len()
        ),
        !pointing.is_empty()
            && pointing.iter().all(|b| *b)
            && form_plan.is_some_and(|p| p.window.as_deref() == Some(FORM))
            && checked == executed.len(),
    );
    if let Some(p) = form_plan {
        checks.note(format!("plan {} in {FORM}, {} steps", p.digest(), p.steps.len()));
    }

    // The record matches field for field.
    let saved = saved.unwrap_or_default();
    let fields = ["name", "email", "phone", "company", "account"];
    let matched = fields
        .iter()
        .filter(|f| {
            saved.get(**f).map(String::as_str) == crm_record.get(f).filter(|v| !v.is_empty())
        })
        .count();
    checks.check(
        &format!("the form's saved customer matches the CRM's record field for field ({matched}/{})", fields.len()),
        matched == fields.len() && saved.len() == fields.len(),
    );

    let answered = tool_responses(&events, COMPUTER_USE_EXECUTE).len();
    checks.note(format!(
        "{} desktop actions through {answered} computer_use_execute calls; {} approvals",
        actions.len(),
        prompts.len()
    ));
    Ok(checks)
}
