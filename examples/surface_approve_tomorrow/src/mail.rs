//! The mail driver both scenarios mount: a stdio MCP server this binary serves when started
//! as `<example> serve-mail-driver`, which the kernel supervises (spec 3.2 §7.7).
//!
//! | Tool | Class in the manifest | Behaviour |
//! |---|---|---|
//! | `send_email` | `send_external`, so `NonIdempotent`: never retried, recorded in the action ledger, and in Ask it waits for a decision | Appends one line to `outbox.jsonl` per execution and syncs it to disk, so the scenario counts sends exactly |
//!
//! It exits when the kernel that started it is gone: on end of input, and when its parent
//! process changes, as it does after `kill -9` of the kernel.

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use rmcp::model::*;
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

/// The file the driver appends each sent email to.
pub const OUTBOX: &str = "outbox.jsonl";

/// Serves the mail driver on standard input and output until the kernel goes away.
pub async fn serve() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let parent = std::os::unix::process::parent_id();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                if std::os::unix::process::parent_id() != parent {
                    std::process::exit(0);
                }
            }
        });
    }
    let running = Mail.serve((tokio::io::stdin(), tokio::io::stdout())).await?;
    let _ = running.waiting().await;
    Ok(())
}

fn object(value: Value) -> Arc<JsonObject> {
    Arc::new(value.as_object().cloned().unwrap_or_default())
}

#[derive(Clone)]
struct Mail;

impl ServerHandler for Mail {
    fn get_info(&self) -> InitializeResult {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _params: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "send_email",
            "Sends an email from the person's account. It cannot be unsent.",
            object(json!({
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "The recipient's address" },
                    "subject": { "type": "string", "description": "The subject line" },
                    "body": { "type": "string", "description": "The message" }
                },
                "required": ["to", "subject", "body"]
            })),
        )]))
    }

    async fn call_tool(
        &self,
        params: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        if params.name.as_ref() != "send_email" {
            let text = format!("no tool {}", params.name);
            return Ok(CallToolResult::error(vec![ContentBlock::text(text)]).into());
        }
        let args = params.arguments.unwrap_or_default();
        let field = |name: &str| args.get(name).and_then(Value::as_str).unwrap_or_default();
        let (to, subject) = (field("to").to_string(), field("subject").to_string());
        let outbox = std::env::var("MAIL_OUTBOX")
            .map_err(|_| ErrorData::internal_error("MAIL_OUTBOX is not set", None))?;
        let line = json!({ "to": to, "subject": subject, "at": chrono::Utc::now() });
        let written = std::fs::OpenOptions::new().create(true).append(true).open(&outbox).and_then(
            |mut file| {
                writeln!(file, "{line}")?;
                file.sync_all()
            },
        );
        written.map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        let text = format!("Sent \"{subject}\" to {to}.");
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}

/// The mail driver's manifest: run by `exe` (this binary) with `instance` on its command
/// line, so a scenario can tell whether it still runs; its outbox in `dir`, which the OS
/// sandbox lets it write.
pub fn manifest(exe: &Path, dir: &Path, instance: &str) -> String {
    let quote = |text: &str| format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""));
    let dir_text = dir.display().to_string();
    format!(
        "id = \"mail\"\n\
         command = {}\n\
         args = [\"serve-mail-driver\", \"--instance\", {}]\n\
         protocol = \"discover\"\n\
         sandbox = \"os\"\n\
         allow_read = [{}]\n\
         allow_write = [{}]\n\
         env = {{ MAIL_OUTBOX = {} }}\n\
         [tools]\n\
         send_email = \"send_external\"\n",
        quote(&exe.display().to_string()),
        quote(instance),
        quote(&dir_text),
        quote(&dir_text),
        quote(&dir.join(OUTBOX).display().to_string()),
    )
}

/// The emails the driver sent, in order.
pub fn sent(dir: &Path) -> Vec<Value> {
    std::fs::read_to_string(dir.join(OUTBOX))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}
