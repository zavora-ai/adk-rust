//! A stdio MCP server the driver tests mount as a driver, the manifests that run it, and a
//! tool context inside a run (spec 3.2 §7.7).
//!
//! A test binary runs itself as the server: the manifest's command is the binary, its
//! arguments select the ignored `stdio_driver` test, and `SERVER_ENV` makes that test
//! serve [`Notes`] on stdin and stdout.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use adk_core::{
    Artifacts, CallbackContext, Content, EventActions, MemoryEntry, ReadonlyContext, RunConfig,
    ToolContext,
};
use adk_kernel::drivers::DriverManifest;
use async_trait::async_trait;
use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

/// Set in the server's environment by the manifest.
pub const SERVER_ENV: &str = "ADK_KERNEL_DRIVER_TEST_SERVER";
/// A file the server appends one line to per execution of `send_note`.
pub const LOG_ENV: &str = "ADK_KERNEL_DRIVER_TEST_LOG";
/// A file whose text the server adds to `read_note`'s description, read at each listing.
pub const DECLARATION_ENV: &str = "ADK_KERNEL_DRIVER_TEST_DECLARATION";

/// Serves [`Notes`] on stdio when [`SERVER_ENV`] is set; returns at once otherwise.
pub async fn serve_stdio() {
    if std::env::var(SERVER_ENV).is_err() {
        return;
    }
    let running = Notes.serve((tokio::io::stdin(), tokio::io::stdout())).await.unwrap();
    let _ = running.waiting().await;
}

fn schema(value: Value) -> Arc<JsonObject> {
    Arc::new(value.as_object().cloned().unwrap_or_default())
}

fn question(message: &str) -> InputRequest {
    InputRequest::Elicitation(ElicitRequest::new(ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: message.into(),
        requested_schema: serde_json::from_value(json!({
            "type": "object",
            "properties": { "ok": { "type": "boolean", "default": true } }
        }))
        .unwrap(),
    }))
}

fn text(result: impl Into<String>) -> CallToolResponse {
    CallToolResult::success(vec![ContentBlock::text(result.into())]).into()
}

/// Notes: a read, a send that asks first (the 2026-07-28 input-required round), a file
/// write, an environment report, and a crash.
#[derive(Clone, Default)]
pub struct Notes;

impl ServerHandler for Notes {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let variant = std::env::var(DECLARATION_ENV)
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| text.trim().to_string())
            .unwrap_or_default();
        let read = if variant.is_empty() {
            "Reads a note.".to_string()
        } else {
            format!("Reads a note ({variant}).")
        };
        let string = json!({ "type": "string" });
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new("read_note", read, schema(json!({ "type": "object" })))
                .with_annotations(ToolAnnotations::new().read_only(true)),
            Tool::new(
                "send_note",
                "Sends a note to a person.",
                schema(json!({ "type": "object", "properties": { "text": string } })),
            ),
            Tool::new(
                "write_file",
                "Writes text to a file.",
                schema(
                    json!({ "type": "object", "properties": { "path": string, "text": string } }),
                ),
            ),
            Tool::new(
                "env_report",
                "Reports the server's environment.",
                schema(json!({ "type": "object" })),
            )
            .with_annotations(ToolAnnotations::new().read_only(true)),
            Tool::new("crash", "Exits the server.", schema(json!({ "type": "object" }))),
        ]))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arg = |name: &str| {
            params
                .arguments
                .as_ref()
                .and_then(|args| args.get(name))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Ok(match params.name.as_ref() {
            "read_note" => text("a note"),
            "send_note" => {
                let (Some(state), Some(responses)) =
                    (&params.request_state, &params.input_responses)
                else {
                    return Ok(InputRequiredResult::new(
                        Some([("confirm".to_string(), question("Send the note?"))].into()),
                        Some("state-1".to_string()),
                    )
                    .into());
                };
                if state != "state-1" || responses["confirm"]["action"] != "accept" {
                    return Ok(text("not sent"));
                }
                if let Ok(path) = std::env::var(LOG_ENV) {
                    use std::io::Write;
                    let mut log =
                        std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
                    writeln!(log, "{}", arg("text")).unwrap();
                }
                text(format!("sent {}", arg("text")))
            }
            "write_file" => match std::fs::write(arg("path"), arg("text")) {
                Ok(()) => text("written"),
                Err(error) => {
                    CallToolResult::error(vec![ContentBlock::text(error.to_string())]).into()
                }
            },
            "env_report" => {
                let env: BTreeMap<String, String> = std::env::vars().collect();
                let files: BTreeMap<String, String> = env
                    .iter()
                    .filter(|(name, _)| name.ends_with("_FILE"))
                    .map(|(name, path)| {
                        (name.clone(), std::fs::read_to_string(path).unwrap_or_default())
                    })
                    .collect();
                text(json!({ "env": env, "files": files }).to_string())
            }
            "crash" => std::process::exit(3),
            other => {
                CallToolResult::error(vec![ContentBlock::text(format!("no tool {other}"))]).into()
            }
        })
    }
}

/// Where one test's server keeps its log and declaration.
pub struct Workdir {
    pub dir: tempfile::TempDir,
}

impl Workdir {
    pub fn new() -> Self {
        Self { dir: tempfile::tempdir().unwrap() }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn log(&self) -> PathBuf {
        self.path().join("executions.log")
    }

    pub fn declaration(&self) -> PathBuf {
        self.path().join("declaration.txt")
    }

    /// The lines the server logged, one per execution of `send_note`.
    pub fn executions(&self) -> Vec<String> {
        std::fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Changes what `read_note` declares from the server's next listing on.
    pub fn redeclare(&self, variant: &str) {
        std::fs::write(self.declaration(), variant).unwrap();
    }

    /// The manifest of driver `id` running this test binary's [`Notes`], sandboxed where an
    /// enforcer exists with the work directory readable and writable; `extra` adds TOML keys
    /// before the tools table.
    pub fn manifest(&self, id: &str, extra: &str) -> DriverManifest {
        let quote = |text: &str| format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""));
        let exe = std::env::current_exe().unwrap().display().to_string();
        let toml = format!(
            "id = \"{id}\"\n\
             command = {}\n\
             args = [\"stdio_driver\", \"--exact\", \"--ignored\", \"--quiet\", \"--test-threads=1\"]\n\
             protocol = \"discover\"\n\
             env = {{ {SERVER_ENV} = \"1\", {LOG_ENV} = {}, {DECLARATION_ENV} = {} }}\n\
             restart = {{ max_attempts = 5, initial_delay_ms = 100, max_delay_ms = 400, backoff_multiplier = 2.0 }}\n\
             allow_read = [{dir}]\n\
             allow_write = [{dir}]\n\
             {extra}\n\
             [tools]\n\
             read_note = \"read\"\n\
             send_note = \"send_external\"\n\
             write_file = \"write_local\"\n\
             env_report = \"read\"\n\
             crash = \"other\"\n",
            quote(&exe),
            quote(&self.log().display().to_string()),
            quote(&self.declaration().display().to_string()),
            dir = quote(&self.path().display().to_string()),
        );
        DriverManifest::from_toml(&toml).unwrap()
    }
}

/// A tool context inside a run of agent `agent` in session `session`.
pub struct RunContext {
    pub run_config: Option<RunConfig>,
    pub call_id: String,
    /// The call's idempotency key, as the agent gives it; derived from the call when unset.
    pub key: Option<String>,
    pub session: String,
    pub content: Content,
    pub actions: Mutex<EventActions>,
}

impl RunContext {
    /// The context of a dispatch under idempotency key `key`.
    pub fn keyed(
        run_config: Option<RunConfig>,
        session: &str,
        call_id: &str,
        key: String,
    ) -> Arc<Self> {
        let mut ctx = Arc::into_inner(Self::new(run_config, session, call_id)).unwrap();
        ctx.key = Some(key);
        Arc::new(ctx)
    }

    pub fn new(run_config: Option<RunConfig>, session: &str, call_id: &str) -> Arc<Self> {
        Arc::new(Self {
            run_config,
            call_id: call_id.to_string(),
            key: None,
            session: session.to_string(),
            content: Content::new("user").with_text("go"),
            actions: Mutex::new(EventActions::default()),
        })
    }
}

#[async_trait]
impl ReadonlyContext for RunContext {
    fn invocation_id(&self) -> &str {
        "inv-1"
    }
    fn agent_name(&self) -> &str {
        "agent"
    }
    fn user_id(&self) -> &str {
        "alice"
    }
    fn app_name(&self) -> &str {
        "adk-rust"
    }
    fn session_id(&self) -> &str {
        &self.session
    }
    fn branch(&self) -> &str {
        ""
    }
    fn user_content(&self) -> &Content {
        &self.content
    }
}

#[async_trait]
impl CallbackContext for RunContext {
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>> {
        None
    }
}

#[async_trait]
impl ToolContext for RunContext {
    fn function_call_id(&self) -> &str {
        &self.call_id
    }
    fn idempotency_key(&self) -> String {
        self.key
            .clone()
            .unwrap_or_else(|| format!("adk-rust/alice/{}/inv-1/{}", self.session, self.call_id))
    }
    fn actions(&self) -> EventActions {
        self.actions.lock().unwrap().clone()
    }
    fn set_actions(&self, actions: EventActions) {
        *self.actions.lock().unwrap() = actions;
    }
    async fn search_memory(&self, _query: &str) -> adk_core::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }
    fn run_config(&self) -> Option<&RunConfig> {
        self.run_config.as_ref()
    }
}
