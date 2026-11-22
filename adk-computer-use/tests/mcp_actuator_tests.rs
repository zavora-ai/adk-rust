//! `McpComputerActuator` against an in-process server with computer-use-mcp's plain tools.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_computer_use::{ComputerUseMember, ComputerUseToolset, McpComputerActuator};
use adk_core::{
    ActuationOutput, AdkError, ComputerAction, ComputerActionKind, ComputerActuator, ErrorCategory,
    MouseButton, ScreenPoint, ScrollDirection,
};
use adk_tool::McpToolset;
use base64::Engine;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, InitializeResult,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

/// The tools computer-use-mcp's default `desktop` profile lists that the actuator uses.
const DESKTOP: &[&str] =
    &["screenshot", "click", "mouse_move", "scroll", "type", "key", "hold_key", "cursor_position"];

/// The tools the `ax` profile adds.
const AX: &[&str] = &["mouse_drag", "left_click_drag", "left_mouse_down", "left_mouse_up"];

/// A 2560×1600-point display captured at 1280×800, as computer-use-mcp states it.
const MAPPING: &str = "1280x800 | screen: display 1 at 0,0 sized 2560x1600 (ScreenCaptureKit)\n\
                       screen_x = 0 + image_x * 2.0000\n\
                       screen_y = 0 + image_y * 2.0000\n\
                       a display capture scales uniformly, so this mapping is exact.";

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

#[derive(Clone)]
struct FakeDesktop {
    tools: Vec<&'static str>,
    calls: Calls,
    refuse: Option<&'static str>,
    mapping: &'static str,
}

impl FakeDesktop {
    fn new(tools: &[&'static str]) -> Self {
        Self { tools: tools.to_vec(), calls: Calls::default(), refuse: None, mapping: MAPPING }
    }
}

/// The first 24 bytes of a `width`×`height` PNG, which carry its size.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut data = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    data.extend_from_slice(&width.to_be_bytes());
    data.extend_from_slice(&height.to_be_bytes());
    data
}

impl ServerHandler for FakeDesktop {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let schema = Arc::new(serde_json::Map::new());
        Ok(ListToolsResult::with_all_items(
            self.tools.iter().map(|name| Tool::new(*name, *name, Arc::clone(&schema))).collect(),
        ))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = params.name.to_string();
        let arguments = Value::Object(params.arguments.unwrap_or_default());
        self.calls.lock().unwrap().push((name.clone(), arguments));
        let result = match name.as_str() {
            refused if self.refuse == Some(refused) => {
                CallToolResult::error(vec![ContentBlock::text(
                    "paused_by_user: the person paused the agent",
                )])
            }
            "screenshot" => CallToolResult::success(vec![
                ContentBlock::image(
                    base64::engine::general_purpose::STANDARD.encode(png(1280, 800)),
                    "image/png",
                ),
                ContentBlock::text(self.mapping),
            ]),
            "cursor_position" => CallToolResult::success(vec![ContentBlock::text("(200, 100)")]),
            _ => CallToolResult::success(vec![ContentBlock::text("ok")]),
        };
        Ok(result.into())
    }
}

async fn serve(desktop: FakeDesktop) -> Arc<McpToolset> {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        if let Ok(running) = desktop.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    Arc::new(McpToolset::new(().serve(client_io).await.unwrap()))
}

async fn connect(desktop: &FakeDesktop) -> McpComputerActuator {
    let actuator =
        McpComputerActuator::builder(serve(desktop.clone()).await).connect().await.unwrap();
    desktop.calls.lock().unwrap().clear();
    actuator
}

fn calls(desktop: &FakeDesktop) -> Vec<(String, Value)> {
    desktop.calls.lock().unwrap().clone()
}

fn call(tool: &str, arguments: Value) -> (String, Value) {
    (tool.to_string(), arguments)
}

fn summary(error: &AdkError) -> (&str, ErrorCategory, &str) {
    (error.code, error.category, error.message.as_str())
}

#[tokio::test]
async fn connect_takes_a_screenshot_that_fixes_the_display_size() {
    let desktop = FakeDesktop::new(DESKTOP);
    let actuator = McpComputerActuator::builder(serve(desktop.clone()).await)
        .display_id(2)
        .connect()
        .await
        .unwrap();

    assert_eq!(actuator.display_size(), Some((1280, 800)));
    assert_eq!(
        calls(&desktop),
        [call(
            "screenshot",
            json!({ "full_screen": true, "width": 1280, "quality": 0, "resend": true, "display_id": 2 })
        )]
    );
}

#[tokio::test]
async fn a_screenshot_is_returned_as_the_png_the_server_sent() {
    let desktop = FakeDesktop::new(DESKTOP);
    let actuator = connect(&desktop).await;

    let ActuationOutput::Image(shot) = actuator.perform(ComputerAction::Screenshot).await.unwrap()
    else {
        panic!("expected an image");
    };

    assert_eq!((shot.mime_type.as_str(), shot.width, shot.height), ("image/png", 1280, 800));
    assert_eq!(shot.data, png(1280, 800));
}

#[tokio::test]
async fn coordinates_are_converted_to_screen_points() {
    let desktop = FakeDesktop::new(&[DESKTOP, AX].concat());
    let actuator = connect(&desktop).await;

    let click = ComputerAction::Click {
        button: MouseButton::Right,
        count: 2,
        at: Some(ScreenPoint::new(100, 50)),
        modifiers: vec!["super".into(), "shift".into()],
    };
    actuator.perform(click).await.unwrap();
    actuator.perform(ComputerAction::MoveTo { at: ScreenPoint::new(640, 400) }).await.unwrap();
    let drag = ComputerAction::Drag {
        from: ScreenPoint::new(10, 20),
        to: ScreenPoint::new(30, 40),
        modifiers: vec!["alt".into()],
    };
    actuator.perform(drag).await.unwrap();

    assert_eq!(
        calls(&desktop),
        [
            call(
                "click",
                json!({
                    "coordinate": [200.0, 100.0],
                    "button": "right",
                    "count": 2,
                    "modifiers": ["cmd", "shift"],
                })
            ),
            call("mouse_move", json!({ "coordinate": [1280.0, 800.0] })),
            call(
                "mouse_drag",
                json!({
                    "path": [[20.0, 40.0], [60.0, 80.0]],
                    "button": "left",
                    "modifiers": ["alt"],
                })
            ),
        ]
    );
}

#[tokio::test]
async fn actions_at_the_cursor_ask_the_server_where_it_is() {
    let desktop = FakeDesktop::new(&[DESKTOP, AX].concat());
    let actuator = connect(&desktop).await;

    let scroll = ComputerAction::Scroll {
        at: None,
        direction: ScrollDirection::Down,
        amount: 3,
        modifiers: Vec::new(),
    };
    actuator.perform(scroll).await.unwrap();
    actuator.perform(ComputerAction::MouseDown).await.unwrap();
    let position = actuator.perform(ComputerAction::CursorPosition).await.unwrap();

    assert_eq!(position, ActuationOutput::Cursor(ScreenPoint::new(100, 50)));
    assert_eq!(
        calls(&desktop),
        [
            call("cursor_position", json!({})),
            call(
                "scroll",
                json!({ "coordinate": [200.0, 100.0], "direction": "down", "amount": 3 })
            ),
            call("cursor_position", json!({})),
            call("left_mouse_down", json!({ "coordinate": [200.0, 100.0] })),
            call("cursor_position", json!({})),
        ]
    );
}

#[tokio::test]
async fn keyboard_actions_use_the_servers_arguments() {
    let desktop = FakeDesktop::new(DESKTOP);
    let actuator = connect(&desktop).await;

    actuator.perform(ComputerAction::Type { text: "torus".into() }).await.unwrap();
    actuator.perform(ComputerAction::Key { keys: "ctrl+s".into(), repeat: 1 }).await.unwrap();
    actuator.perform(ComputerAction::Key { keys: "Tab".into(), repeat: 3 }).await.unwrap();
    let hold = ComputerAction::HoldKey { keys: "shift+a".into(), duration: Duration::from_secs(2) };
    actuator.perform(hold).await.unwrap();

    assert_eq!(
        calls(&desktop),
        [
            call("type", json!({ "text": "torus" })),
            call("key", json!({ "text": "ctrl+s" })),
            call("key", json!({ "text": "Tab", "repeat": 3 })),
            call("hold_key", json!({ "keys": ["shift", "a"], "duration": 2.0 })),
        ]
    );
}

#[tokio::test]
async fn supported_actions_follow_the_tools_the_server_lists() {
    let desktop = FakeDesktop::new(DESKTOP);
    let actuator = connect(&desktop).await;
    let ax = connect(&FakeDesktop::new(&[DESKTOP, AX].concat())).await;
    let kinds = [
        ComputerActionKind::Screenshot,
        ComputerActionKind::Click,
        ComputerActionKind::Drag,
        ComputerActionKind::MouseDown,
        ComputerActionKind::MouseUp,
        ComputerActionKind::Zoom,
        ComputerActionKind::Wait,
    ];

    let supported = |actuator: &McpComputerActuator| {
        kinds.iter().map(|kind| actuator.supports(*kind)).collect::<Vec<_>>()
    };

    assert_eq!(supported(&actuator), [true, true, false, false, false, false, false]);
    assert_eq!(supported(&ax), [true, true, true, true, true, false, false]);
    let drag = ComputerAction::Drag {
        from: ScreenPoint::new(0, 0),
        to: ScreenPoint::new(1, 1),
        modifiers: Vec::new(),
    };
    let error = actuator.perform(drag).await.unwrap_err();
    assert_eq!(
        summary(&error),
        (
            "computer_use.mcp.unsupported_action",
            ErrorCategory::Unsupported,
            "the server lists no tool for Drag actions",
        )
    );
    assert_eq!(calls(&desktop), []);
}

#[tokio::test]
async fn the_governed_toolset_offers_only_the_supported_members() {
    let desktop = FakeDesktop::new(DESKTOP);
    let toolset = ComputerUseToolset::new(Arc::new(connect(&desktop).await));

    assert_eq!(
        toolset.members(),
        [
            ComputerUseMember::Screenshot,
            ComputerUseMember::LeftClick,
            ComputerUseMember::RightClick,
            ComputerUseMember::MiddleClick,
            ComputerUseMember::DoubleClick,
            ComputerUseMember::TripleClick,
            ComputerUseMember::MouseMove,
            ComputerUseMember::CursorPosition,
            ComputerUseMember::Scroll,
            ComputerUseMember::Type,
            ComputerUseMember::Key,
            ComputerUseMember::HoldKey,
            ComputerUseMember::Wait,
        ]
    );
}

#[tokio::test]
async fn a_server_refusal_reaches_the_caller() {
    let desktop = FakeDesktop { refuse: Some("click"), ..FakeDesktop::new(DESKTOP) };
    let actuator = connect(&desktop).await;

    let click = ComputerAction::Click {
        button: MouseButton::Left,
        count: 1,
        at: Some(ScreenPoint::new(1, 1)),
        modifiers: Vec::new(),
    };
    let error = actuator.perform(click).await.unwrap_err();

    assert_eq!(
        summary(&error),
        (
            "computer_use.mcp.action_failed",
            ErrorCategory::InvalidInput,
            "computer-use-mcp `click` failed: paused_by_user: the person paused the agent",
        )
    );
}

#[tokio::test]
async fn invalid_actions_are_refused_before_reaching_the_server() {
    let desktop = FakeDesktop::new(DESKTOP);
    let actuator = connect(&desktop).await;

    let click = ComputerAction::Click {
        button: MouseButton::Left,
        count: 1,
        at: Some(ScreenPoint::new(1, 1)),
        modifiers: vec!["hyper".into()],
    };
    let modifier = actuator.perform(click).await.unwrap_err();
    let hold = ComputerAction::HoldKey { keys: "w".into(), duration: Duration::from_secs(30) };
    let long_hold = actuator.perform(hold).await.unwrap_err();

    assert_eq!(
        [summary(&modifier), summary(&long_hold)],
        [
            (
                "computer_use.mcp.invalid_action",
                ErrorCategory::InvalidInput,
                "unknown modifier \"hyper\"; expected shift, ctrl, alt or super",
            ),
            (
                "computer_use.mcp.invalid_action",
                ErrorCategory::InvalidInput,
                "hold_key lasts 30 s; computer-use-mcp holds keys for more than 0 and at most 10 s",
            ),
        ]
    );
    assert_eq!(calls(&desktop), []);
}

#[tokio::test]
async fn connect_refuses_a_server_it_cannot_drive() {
    let no_screenshot = FakeDesktop::new(&["click"]);
    let no_mapping = FakeDesktop { mapping: "1280x800", ..FakeDesktop::new(DESKTOP) };

    let first = McpComputerActuator::builder(serve(no_screenshot).await).connect().await;
    let second = McpComputerActuator::builder(serve(no_mapping).await).connect().await;

    assert_eq!(
        first.err().map(|error| (error.code, error.category)),
        Some(("computer_use.mcp.no_screenshot", ErrorCategory::Unsupported))
    );
    assert_eq!(
        second.err().map(|error| (error.code, error.category)),
        Some(("computer_use.mcp.action_failed", ErrorCategory::InvalidInput))
    );
}
