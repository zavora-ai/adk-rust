//! The function tools the scenarios give their agents, with logs of the calls that ran.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use adk_code::{
    CodeExecutor, ContainerCommandExecutor, ContainerConfig, ExecutionLanguage, ExecutionPayload,
    ExecutionRequest, SandboxPolicy,
};
use adk_core::{Result, Tool, ToolContext};
use adk_tool::{AdkError, FunctionTool, tool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

type BoxedTool = Arc<dyn Tool>;

/// The `count` of every `reserve_seats` call that ran.
pub static SEATS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SeatArgs {
    /// How many seats to reserve.
    pub count: u32,
}

/// Reserves seats on the next shuttle.
#[tool]
pub async fn reserve_seats(args: SeatArgs) -> std::result::Result<Value, AdkError> {
    if let Ok(mut seats) = SEATS.lock() {
        seats.push(args.count);
    }
    Ok(json!({ "reserved": args.count }))
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WidgetOrder {
    /// Number of widgets. Widgets ship in boxes of five.
    #[schemars(extend("multipleOf" = 5))]
    pub quantity: u32,
}

/// A `FunctionTool` whose schema only accepts multiples of five; every quantity it ran
/// with is logged.
pub fn order_widgets(log: Arc<Mutex<Vec<Value>>>) -> BoxedTool {
    Arc::new(
        FunctionTool::new(
            "order_widgets",
            "Orders widgets from the warehouse.",
            move |_ctx: Arc<dyn ToolContext>, args: Value| {
                let log = log.clone();
                async move {
                    if let Ok(mut log) = log.lock() {
                        log.push(args.get("quantity").cloned().unwrap_or(Value::Null));
                    }
                    Ok(json!({ "status": "ordered", "quantity": args.get("quantity") }))
                }
            },
        )
        .with_parameters_schema::<WidgetOrder>(),
    )
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ClockArgs {
    /// IANA time zone, such as `Africa/Nairobi`.
    pub zone: Option<String>,
}

/// `get_time`, with a parameters schema so its declaration is normalized like any other.
pub fn clock(log: Arc<Mutex<Vec<Value>>>) -> BoxedTool {
    Arc::new(
        FunctionTool::new(
            "get_time",
            "Returns the current time.",
            move |_ctx: Arc<dyn ToolContext>, args: Value| {
                let log = log.clone();
                async move {
                    if let Ok(mut log) = log.lock() {
                        log.push(args);
                    }
                    Ok(json!({ "time": "10:42" }))
                }
            },
        )
        .with_parameters_schema::<ClockArgs>(),
    )
}

/// A plain tool that answers with `answer` and logs its calls.
pub fn answering_tool(
    name: &'static str,
    description: &'static str,
    answer: Value,
    log: Arc<Mutex<Vec<Value>>>,
) -> BoxedTool {
    Arc::new(FunctionTool::new(
        name,
        description,
        move |_ctx: Arc<dyn ToolContext>, args: Value| {
            let log = log.clone();
            let answer = answer.clone();
            async move {
                if let Ok(mut log) = log.lock() {
                    log.push(args);
                }
                Ok(answer)
            }
        },
    ))
}

/// The JSON Schema `file_ticket` declares: a boolean subschema, a numeric enum, a
/// nullable string, a two-type union and a map.
pub fn ticket_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "priority": { "type": "integer", "enum": [1, 2, 3], "description": "1 is urgent." },
            "note": { "type": ["string", "null"], "description": "Optional note." },
            "reference": {
                "description": "Ticket reference, a code or a number.",
                "anyOf": [{ "type": "string" }, { "type": "integer" }]
            },
            "labels": {
                "type": "object",
                "description": "Label counts.",
                "additionalProperties": { "type": "integer" }
            },
            "extra": true
        },
        "required": ["priority"]
    })
}

/// A hand-written tool declaring [`ticket_schema`].
pub struct FileTicket {
    pub calls: Arc<Mutex<Vec<Value>>>,
}

#[adk_core::async_trait]
impl Tool for FileTicket {
    fn name(&self) -> &str {
        "file_ticket"
    }

    fn description(&self) -> &str {
        "Files a support ticket."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(ticket_schema())
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(args);
        }
        Ok(json!({ "ticket": "T-901" }))
    }
}

/// A tool that runs Python through `ContainerCommandExecutor` with a three-second timeout.
pub fn run_python(runtime: PathBuf) -> BoxedTool {
    let executor = Arc::new(ContainerCommandExecutor::new(ContainerConfig {
        runtime: runtime.display().to_string(),
        ..ContainerConfig::default()
    }));
    Arc::new(
        FunctionTool::new(
            "run_python",
            "Runs a Python program in a container and returns its output.",
            move |_ctx: Arc<dyn ToolContext>, args: Value| {
                let executor = executor.clone();
                async move {
                    let code = args.get("code").and_then(Value::as_str).unwrap_or_default();
                    let mut sandbox = SandboxPolicy::strict_rust();
                    sandbox.timeout = Duration::from_secs(3);
                    let result = executor
                        .execute(ExecutionRequest {
                            language: ExecutionLanguage::Python,
                            payload: ExecutionPayload::Source { code: code.to_string() },
                            argv: vec![],
                            stdin: None,
                            input: None,
                            sandbox,
                            identity: None,
                        })
                        .await
                        .map_err(|error| AdkError::tool(format!("execution failed: {error}")))?;
                    Ok(json!({ "status": result.status, "stdout": result.stdout }))
                }
            },
        )
        .with_parameters_schema::<PythonArgs>(),
    )
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PythonArgs {
    /// Python source code.
    pub code: String,
}
