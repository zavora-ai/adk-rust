//! Claude's computer use toolset (`computer_toolset_20260801`) with `zoom` on the S5
//! screenshot, through `FittedActuator` (spec 3.7 §7.14, X7).
//!
//! The desktop is the 3024×1964 DPR-2 fixture; `FittedActuator` sends Claude a 2380×1546
//! copy, crops `zoom` regions from the full-resolution capture and scales every coordinate
//! back. The live test asks `claude-sonnet-5-5` to zoom on the small help button and click
//! Save, and records the calls it made in `tests/fixtures/screen/claude-zoom.json` when
//! `ADK_VISION_RECORD=1`; the replay test runs the recorded calls through the same stack
//! without a key.

use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_computer_use::ComputerUseToolset;
use adk_core::{
    ActuationOutput, ComputerAction, ComputerActionKind, ComputerActuator, Content, Part,
    ScreenPoint, Screenshot,
};
use adk_runner::Runner;
use adk_session::{CreateRequest, InMemorySessionService, SessionService};
use adk_vision::screen::{FittedActuator, ScreenshotScale};
use adk_vision::{BBox, ScreenSpace};
use futures::StreamExt;
use serde_json::{Value, json};

const SCREEN_PNG: &[u8] = include_bytes!("fixtures/s5/s5-screen.png");
const RECORDING: &str = "tests/fixtures/screen/claude-zoom.json";
const MODEL: &str = "claude-sonnet-5-5";
const HELP: BBox = BBox { x0: 40.0, y0: 1880.0, x1: 160.0, y1: 1950.0 };
const SAVE: BBox = BBox { x0: 1920.0, y0: 1180.0, x1: 2100.0, y1: 1260.0 };

/// The S5 desktop: every screenshot is the fixture; it records what it is asked to do.
#[derive(Default)]
struct Desktop {
    actions: Mutex<Vec<ComputerAction>>,
}

#[async_trait::async_trait]
impl ComputerActuator for Desktop {
    fn display_size(&self) -> Option<(u32, u32)> {
        Some((3024, 1964))
    }

    fn supports(&self, kind: ComputerActionKind) -> bool {
        kind != ComputerActionKind::Zoom
    }

    async fn perform(&self, action: ComputerAction) -> adk_core::Result<ActuationOutput> {
        self.actions.lock().unwrap().push(action.clone());
        Ok(match action {
            ComputerAction::Screenshot => {
                ActuationOutput::Image(Screenshot::png(SCREEN_PNG.to_vec()).unwrap())
            }
            ComputerAction::CursorPosition => ActuationOutput::Cursor(ScreenPoint::new(0, 0)),
            _ => ActuationOutput::Done,
        })
    }
}

/// Checks what reached the desktop: a zoom region (cropped from a screenshot) covering the
/// help button, and a click inside Save in source pixels and in points.
fn assert_desktop(actions: &[ComputerAction], zooms: &[[i64; 4]], scale: &ScreenshotScale) {
    assert!(!zooms.is_empty(), "Claude zoomed");
    let zoom = scale.region_to_source(*zooms.last().unwrap());
    let covered = BBox::new(zoom[0] as f32, zoom[1] as f32, zoom[2] as f32, zoom[3] as f32);
    let overlap = covered.intersection(&HELP).map_or(0.0, |i| i.area());
    assert!(overlap > 0.5 * HELP.area(), "zoom {zoom:?} covers the help button {HELP:?}");
    let click = actions
        .iter()
        .rev()
        .find_map(|action| match action {
            ComputerAction::Click { at: Some(at), .. } => Some(*at),
            _ => None,
        })
        .expect("Claude clicked");
    let pixel = [click.x as f32, click.y as f32];
    assert!(SAVE.contains(pixel), "click {click:?} lands inside Save {SAVE:?}");
    let screen = ScreenSpace::new("built-in", [0.0, 0.0], 2.0);
    assert!(screen.bbox_to_points(&SAVE).contains(screen.to_points(pixel)));
}

struct ReplayCtx {
    content: Content,
}

#[async_trait::async_trait]
impl adk_core::ReadonlyContext for ReplayCtx {
    fn invocation_id(&self) -> &str {
        "replay"
    }
    fn agent_name(&self) -> &str {
        "screen"
    }
    fn user_id(&self) -> &str {
        "ana"
    }
    fn app_name(&self) -> &str {
        "screen"
    }
    fn session_id(&self) -> &str {
        "s1"
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait::async_trait]
impl adk_core::CallbackContext for ReplayCtx {
    fn artifacts(&self) -> Option<Arc<dyn adk_core::Artifacts>> {
        None
    }
}

#[async_trait::async_trait]
impl adk_core::ToolContext for ReplayCtx {
    fn function_call_id(&self) -> &str {
        "replay-call"
    }
    fn actions(&self) -> adk_core::EventActions {
        adk_core::EventActions::default()
    }
    fn set_actions(&self, _actions: adk_core::EventActions) {}
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<adk_core::MemoryEntry>> {
        Ok(vec![])
    }
}

async fn session() -> Arc<InMemorySessionService> {
    let sessions = Arc::new(InMemorySessionService::new());
    sessions
        .create(CreateRequest {
            app_name: "screen".into(),
            user_id: "ana".into(),
            session_id: Some("s1".into()),
            state: Default::default(),
        })
        .await
        .unwrap();
    sessions
}

#[tokio::test]
#[ignore = "live: needs ANTHROPIC_API_KEY"]
async fn live_claude_zooms_with_the_toolset_and_clicks_inside_save() {
    let Some(key) = std::env::var("ANTHROPIC_API_KEY").ok().filter(|k| !k.is_empty()) else {
        return;
    };
    let desktop = Arc::new(Desktop::default());
    let fitted = Arc::new(FittedActuator::for_claude_model(desktop.clone(), MODEL));
    let toolset = ComputerUseToolset::new(fitted.clone());
    let config = adk_model::anthropic::AnthropicConfig::new(key, MODEL)
        .with_effort(adk_model::anthropic::Effort::Low);
    let model = Arc::new(adk_model::AnthropicClient::new(config).unwrap());
    let agent = LlmAgentBuilder::new("screen")
        .model(model)
        .instruction(
            "You operate a desktop through the computer tools. Take a screenshot first. The \
             label of the small button in the bottom-left corner is too small to read in the \
             screenshot: use zoom on that button to read it. Then click the SAVE button once \
             and stop. Answer with the small button's label.",
        )
        .toolset(Arc::new(toolset))
        .build()
        .unwrap();
    let runner = Runner::builder()
        .app_name("screen")
        .agent(Arc::new(agent))
        .session_service(session().await)
        .build()
        .unwrap();
    let mut stream = runner
        .run(
            adk_core::UserId::try_from("ana").unwrap(),
            adk_core::SessionId::try_from("s1").unwrap(),
            Content::new("user").with_text("Read the small button's label, then save the form."),
        )
        .await
        .unwrap();
    let mut calls = Vec::new();
    let mut answer = String::new();
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        for part in event.llm_response.content.iter().flat_map(|c| c.parts.iter()) {
            match part {
                Part::FunctionCall { name, args, .. } => {
                    calls.push(json!({ "name": name, "args": args }));
                }
                Part::Text { text } if event.author == "screen" && !event.llm_response.partial => {
                    answer.push_str(text);
                }
                _ => {}
            }
        }
    }
    let scale = fitted.scale().unwrap();
    let zooms: Vec<[i64; 4]> = calls
        .iter()
        .filter(|c| c["name"] == "computer.zoom")
        .filter_map(|c| serde_json::from_value(c["args"]["region"].clone()).ok())
        .collect();
    println!("calls: {calls:#?}\nanswer: {answer}\nscale: {scale:?}");
    let actions = desktop.actions.lock().unwrap().clone();
    if std::env::var("ADK_VISION_RECORD").is_ok_and(|v| v == "1") {
        let record = json!({
            "model": MODEL,
            "tool": "computer_toolset_20260801",
            "recorded": chrono::Utc::now().to_rfc3339(),
            "screenshot": { "source": scale.source, "sent": scale.sent, "devicePixelRatio": 2.0 },
            "calls": calls,
            "answer": answer,
        });
        std::fs::create_dir_all("tests/fixtures/screen").unwrap();
        std::fs::write(RECORDING, serde_json::to_string_pretty(&record).unwrap() + "\n").unwrap();
    }
    assert_eq!(scale.sent, [2380, 1546]);
    assert_desktop(&actions, &zooms, &scale);
    assert!(answer.to_uppercase().contains("HELP"), "Claude read the zoomed label: {answer}");
}

/// The recorded live calls, replayed through the toolset and `FittedActuator` without a key.
#[tokio::test]
async fn recorded_claude_zoom_and_click_map_to_their_targets() {
    let Ok(text) = std::fs::read_to_string(RECORDING) else {
        panic!("{RECORDING} is missing; record it with the live test and ADK_VISION_RECORD=1");
    };
    let record: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(record["model"], MODEL);
    assert_eq!(record["screenshot"]["sent"], json!([2380, 1546]));
    let desktop = Arc::new(Desktop::default());
    let fitted = Arc::new(FittedActuator::for_claude_model(desktop.clone(), MODEL));
    let tools = ComputerUseToolset::new(fitted.clone()).member_tools();
    let tool = |name: &str| tools.iter().find(|t| t.name() == name).unwrap().clone();
    let ctx: Arc<dyn adk_core::ToolContext> = Arc::new(ReplayCtx { content: Content::new("user") });
    let mut zooms = Vec::new();
    for call in record["calls"].as_array().unwrap() {
        let name = call["name"].as_str().unwrap();
        if name == "computer.zoom" {
            zooms.push(serde_json::from_value::<[i64; 4]>(call["args"]["region"].clone()).unwrap());
        }
        tool(name).execute(ctx.clone(), call["args"].clone()).await.unwrap();
    }
    let actions = desktop.actions.lock().unwrap().clone();
    assert_desktop(&actions, &zooms, &fitted.scale().unwrap());
}
