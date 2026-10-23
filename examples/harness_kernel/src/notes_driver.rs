//! The notes driver the K6 and parked-question scenarios mount: a stdio MCP server this
//! binary serves when started as `harness-kernel-example serve-notes-driver`.
//!
//! | Tool | Class in the scenarios' manifest | Behaviour |
//! |---|---|---|
//! | `read_note` | `read` | Answers the note's codeword; its description gains the text of `NOTES_DECLARATION` when that file exists, which is how a scenario redeclares it |
//! | `send_note` | `send_external` | Asks "Send the note?" first (the 2026-07-28 input-required round), then appends the text to `NOTES_LOG` once per execution |

use std::sync::Arc;

use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

/// The codeword `read_note` answers with.
pub const CODEWORD: &str = "kestrel-42";

/// Serves the notes driver on stdin and stdout until the client goes away.
pub async fn serve() -> anyhow::Result<()> {
    let running = Notes.serve((tokio::io::stdin(), tokio::io::stdout())).await?;
    let _ = running.waiting().await;
    Ok(())
}

fn object(value: Value) -> Arc<JsonObject> {
    Arc::new(value.as_object().cloned().unwrap_or_default())
}

fn text(result: impl Into<String>) -> CallToolResponse {
    CallToolResult::success(vec![ContentBlock::text(result.into())]).into()
}

#[derive(Clone)]
struct Notes;

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
        let variant = std::env::var("NOTES_DECLARATION")
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty());
        let read = match variant {
            Some(variant) => format!("Reads the shared note and answers its codeword ({variant})."),
            None => "Reads the shared note and answers its codeword.".to_string(),
        };
        Ok(ListToolsResult::with_all_items(vec![
            Tool::new("read_note", read, object(json!({ "type": "object" })))
                .with_annotations(ToolAnnotations::new().read_only(true)),
            Tool::new(
                "send_note",
                "Sends a note with the given text to the team.",
                object(json!({
                    "type": "object",
                    "properties": { "text": { "type": "string", "description": "The note" } },
                    "required": ["text"]
                })),
            ),
        ]))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        Ok(match params.name.as_ref() {
            "read_note" => text(format!("The note's codeword is {CODEWORD}.")),
            "send_note" => {
                let note = params
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let (Some(state), Some(responses)) = (&params.request_state, &params.input_responses)
                else {
                    let schema = serde_json::from_value(json!({
                        "type": "object",
                        "properties": { "ok": { "type": "boolean", "default": true } }
                    }))
                    .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
                    let question = InputRequest::Elicitation(ElicitRequest::new(
                        ElicitRequestParams::FormElicitationParams {
                            meta: None,
                            message: format!("Send the note \"{note}\" to the team?"),
                            requested_schema: schema,
                        },
                    ));
                    return Ok(InputRequiredResult::new(
                        Some([("confirm".to_string(), question)].into()),
                        Some("asked-once".to_string()),
                    )
                    .into());
                };
                if state != "asked-once" || responses["confirm"]["action"] != "accept" {
                    return Ok(text("The note was not sent."));
                }
                if let Ok(path) = std::env::var("NOTES_LOG") {
                    use std::io::Write;
                    let mut log = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
                    writeln!(log, "{note}").map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
                }
                text(format!("Sent the note \"{note}\"."))
            }
            other => CallToolResult::error(vec![ContentBlock::text(format!("no tool {other}"))]).into(),
        })
    }
}

/// The manifest of the notes driver run by `exe` (this binary), keeping its log and its
/// declaration file in `dir`, sandboxed where the platform allows with `dir` writable.
pub fn manifest(exe: &std::path::Path, dir: &std::path::Path, instance: &str) -> String {
    let quote = |text: &str| format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""));
    let dir_text = dir.display().to_string();
    format!(
        "id = \"notes\"\n\
         command = {}\n\
         args = [\"serve-notes-driver\", \"--instance\", {}]\n\
         protocol = \"discover\"\n\
         sandbox = \"os\"\n\
         allow_read = [{}]\n\
         allow_write = [{}]\n\
         env = {{ NOTES_LOG = {}, NOTES_DECLARATION = {} }}\n\
         restart = {{ max_attempts = 5, initial_delay_ms = 200, max_delay_ms = 2000, backoff_multiplier = 2.0 }}\n\
         [tools]\n\
         read_note = \"read\"\n\
         send_note = \"send_external\"\n",
        quote(&exe.display().to_string()),
        quote(instance),
        quote(&dir_text),
        quote(&dir_text),
        quote(&dir.join("notes.log").display().to_string()),
        quote(&dir.join("declaration.txt").display().to_string()),
    )
}

/// Kills the notes driver process started with `instance`, as a crash would.
pub fn kill(instance: &str) -> anyhow::Result<()> {
    let status = std::process::Command::new("pkill").args(["-KILL", "-f", instance]).status()?;
    anyhow::ensure!(status.success(), "no notes driver process with instance {instance}");
    Ok(())
}
