mod adapter;
pub mod ag_ui;
pub mod mcp_apps;
pub mod surface;

pub use adapter::{A2uiAdapter, AgUiAdapter, McpAppsAdapter, UiProtocolAdapter};
pub use ag_ui::{
    ADK_UI_SURFACE_EVENT_NAME, AgUiCustomEvent, AgUiErrorEvent, AgUiEvent, AgUiEventType,
    AgUiStateDeltaEvent, AgUiStateSnapshotEvent, AgUiStepEvent, AgUiTextMessageDeltaEvent,
    AgUiTextMessageEndEvent, AgUiTextMessageStartEvent, AgUiToolCallArgsEvent,
    AgUiToolCallEndEvent, AgUiToolCallResultEvent, AgUiToolCallStartEvent, error_event,
    state_delta_event, state_snapshot_event, step_finished_event, step_started_event,
    surface_to_event_stream, text_message_events, tool_call_events,
};
pub use mcp_apps::{
    MCP_APPS_HTML_MIME_TYPE, McpAppsRenderOptions, McpAppsSurfacePayload, McpToolVisibility,
    surface_to_mcp_apps_payload,
};
pub use surface::{UiProtocol, UiSurface};
