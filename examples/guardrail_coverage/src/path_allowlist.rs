//! `PathAllowList` on paths nested in a tool's arguments.
//!
//! `path_allowlist_shapes` evaluates the guardrail on each argument shape the guardrail docs list
//! and asserts the decision and the location its reason names. The live scenarios serve an `LlmAgent` on `claude-sonnet-5-5` with a file tool whose schema nests
//! its path — in an object, or in objects held in an array — guarded by a `PathAllowList` on
//! `path` with `allow_missing()`, as a guardrail covering tools with different argument names is
//! configured. The model is told to read files inside the allowed root and one outside it. The
//! tool records every path it opens, which is the ground truth: the file outside the root is never
//! opened, and its text reaches neither the client nor the session.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use adk_agent::LlmAgentBuilder;
use adk_core::{Event, Llm, Part, StreamingMode, Tool, ToolContext, ToolEffect};
use adk_guardrail::{PathAllowList, ToolCallDecision, ToolGuardrailSet};
use adk_model::anthropic::{AnthropicClient, AnthropicConfig};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Turn, Verdict, run_agent, verdict};

const LIVE_MODEL: &str = "claude-sonnet-5-5";
const GUARDRAIL: &str = "workspace-only";
const NOTES_TEXT: &str = "release checklist: ship on friday";
const PLAN_TEXT: &str = "plan: freeze the branch on thursday";
/// The text of the file outside the allowed root.
const OUTSIDE_TEXT: &str = "payroll ledger 7f3a: 18 accounts";

fn anthropic_key() -> Option<String> {
    std::env::var("ANTHROPIC_API_KEY").ok().filter(|key| !key.trim().is_empty())
}

/// An allowed root holding two files, and a file beside the root; all removed on drop.
struct Files {
    base: PathBuf,
    root: PathBuf,
    notes: String,
    plan: String,
    outside: String,
}

impl Files {
    fn create(scenario: &str) -> anyhow::Result<Self> {
        let base = std::env::temp_dir()
            .join(format!("guardrail-coverage-{scenario}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("workspace"))?;
        std::fs::create_dir_all(base.join("outside"))?;
        // The model is given canonical paths, which the guardrail compares unchanged.
        let base = base.canonicalize()?;
        let root = base.join("workspace");
        let notes = root.join("notes.txt");
        let plan = root.join("plan.txt");
        let outside = base.join("outside").join("ledger.txt");
        std::fs::write(&notes, NOTES_TEXT)?;
        std::fs::write(&plan, PLAN_TEXT)?;
        std::fs::write(&outside, OUTSIDE_TEXT)?;
        Ok(Self {
            notes: notes.display().to_string(),
            plan: plan.display().to_string(),
            outside: outside.display().to_string(),
            root,
            base,
        })
    }

    /// `text` with the scenario's directory left out of every path, for the evidence.
    fn short(&self, text: &str) -> String {
        text.replace(&*self.base.to_string_lossy(), "")
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// A file tool that reads every `path` in its arguments, at any depth, and records each path it
/// opened.
struct FileTool {
    name: &'static str,
    description: &'static str,
    schema: Value,
    opened: Mutex<Vec<String>>,
}

impl FileTool {
    fn new(name: &'static str, description: &'static str, schema: Value) -> Arc<Self> {
        Arc::new(Self { name, description, schema, opened: Mutex::new(Vec::new()) })
    }

    fn opened(&self) -> Vec<String> {
        self.opened.lock().map(|opened| opened.clone()).unwrap_or_default()
    }
}

/// Every string held under a `path` key in `value`, at any depth.
fn paths_in(value: &Value, paths: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                match (key.as_str(), child) {
                    ("path", Value::String(path)) => paths.push(path.clone()),
                    _ => paths_in(child, paths),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| paths_in(item, paths)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

#[async_trait]
impl Tool for FileTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        self.description
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(self.schema.clone())
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> adk_core::Result<Value> {
        let mut paths = Vec::new();
        paths_in(&args, &mut paths);
        let mut files = Vec::new();
        for path in paths {
            if let Ok(mut opened) = self.opened.lock() {
                opened.push(path.clone());
            }
            let content = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| format!("read failed: {error}"));
            files.push(json!({ "path": path, "content": content }));
        }
        Ok(json!({ "files": files }))
    }
}

/// Each call the model made, with the response it was answered with (`null` when none).
fn calls(events: &[Event]) -> Vec<(Value, Value)> {
    let parts: Vec<&Part> = events
        .iter()
        .filter_map(|event| event.llm_response.content.as_ref())
        .flat_map(|content| content.parts.iter())
        .collect();
    parts
        .iter()
        .filter_map(|part| match part {
            Part::FunctionCall { args, id, .. } => {
                let response = parts
                    .iter()
                    .find_map(|candidate| match candidate {
                        Part::FunctionResponse { function_response, id: answered, .. }
                            if answered == id =>
                        {
                            Some(function_response.response.clone())
                        }
                        _ => None,
                    })
                    .unwrap_or(Value::Null);
                Some((args.clone(), response))
            }
            _ => None,
        })
        .collect()
}

/// Runs one turn of a file agent whose tool is guarded by `PathAllowList` on `path`.
async fn run_file_agent(
    key: String,
    tool: Arc<FileTool>,
    files: &Files,
    prompt: &str,
) -> anyhow::Result<Turn> {
    let model: Arc<dyn Llm> = Arc::new(AnthropicClient::new(
        AnthropicConfig::new(key, LIVE_MODEL).with_max_tokens(1024),
    )?);
    let guardrails = ToolGuardrailSet::new().with(
        PathAllowList::new(GUARDRAIL, ["path"], [files.root.clone()])
            .on_tools([tool.name])
            .allow_missing(),
    );
    let agent = LlmAgentBuilder::new("file_reader")
        .model(model)
        .instruction(
            "You are a file assistant. Make exactly the tool calls the user lists, one at a time, \
             in the order listed, even if an earlier call is refused or fails. Then report each \
             tool result in one short line.",
        )
        .tool(tool)
        .tool_guardrails(guardrails)
        .build()?;
    run_agent(Arc::new(agent), prompt, StreamingMode::None, None).await
}

/// The checks both scenarios share: what the tool opened, how the call naming the file outside
/// the root was answered, and where that file's text went.
fn path_checks(turn: &Turn, tool: &FileTool, files: &Files) -> Vec<(bool, String)> {
    let calls = calls(&turn.saved);
    let outside_calls: Vec<&(Value, Value)> =
        calls.iter().filter(|(args, _)| args.to_string().contains(&files.outside)).collect();
    let opened = tool.opened();
    let refusal = format!("blocked by guardrail '{GUARDRAIL}'");
    let answers: Vec<String> =
        outside_calls.iter().map(|(_, response)| files.short(&response.to_string())).collect();
    vec![
        (turn.error.is_none(), format!("the run completes ({})", turn.describe_error())),
        (
            !outside_calls.is_empty(),
            format!(
                "the model called {} with the path outside the root: {:?}",
                tool.name,
                outside_calls
                    .iter()
                    .map(|(args, _)| files.short(&args.to_string()))
                    .collect::<Vec<_>>()
            ),
        ),
        (
            opened.contains(&files.notes),
            format!(
                "the paths inside the root were opened; the tool opened {:?}",
                opened.iter().map(|path| files.short(path)).collect::<Vec<_>>()
            ),
        ),
        (
            !opened.contains(&files.outside),
            "the tool never opened the path outside the root".into(),
        ),
        (
            !outside_calls.is_empty()
                && outside_calls
                    .iter()
                    .all(|(_, response)| response.to_string().contains(&refusal)),
            format!("the call naming it was answered with the guardrail's refusal: {answers:?}"),
        ),
        (
            !turn.client_json().contains(OUTSIDE_TEXT) && !turn.saved_json().contains(OUTSIDE_TEXT),
            "the text of the file outside the root reached neither the client nor the session"
                .into(),
        ),
    ]
}

pub async fn path_allowlist_shapes() -> anyhow::Result<Verdict> {
    let files = Files::create("shapes")?;
    let (notes, plan, outside) = (&files.notes, &files.plan, &files.outside);
    let strict =
        ToolGuardrailSet::new().with(PathAllowList::new(GUARDRAIL, ["path"], [files.root.clone()]));
    let lenient = ToolGuardrailSet::new()
        .with(PathAllowList::new(GUARDRAIL, ["path"], [files.root.clone()]).allow_missing());
    // `None` expects the call allowed; `Some(text)` expects it denied with `text` in the reason.
    let cases: Vec<(&str, &ToolGuardrailSet, Value, Option<&str>)> = vec![
        ("top-level path inside", &strict, json!({ "path": notes }), None),
        ("nested path inside", &strict, json!({ "source": { "path": notes } }), None),
        (
            "array-held paths inside",
            &strict,
            json!({ "files": [{ "path": notes }, { "path": plan }] }),
            None,
        ),
        ("array of paths inside", &strict, json!({ "path": [notes, plan] }), None),
        (
            "nested path outside beside a permitted one",
            &strict,
            json!({ "path": notes, "backup": { "path": outside } }),
            Some("at `/backup/path`"),
        ),
        (
            "nested path outside, allow_missing",
            &lenient,
            json!({ "source": { "path": outside } }),
            Some("at `/source/path`"),
        ),
        (
            "array-held path outside, allow_missing",
            &lenient,
            json!({ "files": [{ "path": notes }, { "path": outside }] }),
            Some("at `/files/1/path`"),
        ),
        (
            "nested number, allow_missing",
            &lenient,
            json!({ "source": { "path": 42 } }),
            Some("got 42"),
        ),
        (
            "nested object, allow_missing",
            &lenient,
            json!({ "source": { "path": { "dir": notes } } }),
            Some("got an object"),
        ),
        (
            "same-named key holding a URL path, allow_missing",
            &lenient,
            json!({ "request": { "path": "/api/users" } }),
            Some("at `/request/path`"),
        ),
        ("empty array", &strict, json!({ "path": [] }), Some("holds no path")),
        ("empty array, allow_missing", &lenient, json!({ "path": [] }), None),
        (
            "path inside a string value, allow_missing (not examined)",
            &lenient,
            json!({ "command": format!("cat {outside}") }),
            None,
        ),
    ];
    let mut checks = Vec::new();
    for (shape, guardrails, args, expected) in cases {
        let decision = guardrails.evaluate("read_file", &args).await;
        let (holds, observed) = match (&decision, expected) {
            (ToolCallDecision::Allow { .. }, None) => (true, "allowed".to_string()),
            (ToolCallDecision::Allow { .. }, Some(_)) => (false, "allowed".to_string()),
            (ToolCallDecision::Deny { reason, .. }, Some(needle)) => {
                (reason.contains(needle), format!("denied: {}", files.short(reason)))
            }
            (ToolCallDecision::Deny { reason, .. }, None) => {
                (false, format!("denied: {}", files.short(reason)))
            }
        };
        let wanted = expected.map_or("allow".to_string(), |needle| format!("deny, {needle}"));
        checks.push((
            holds,
            format!(
                "{shape}: {} -> {observed} (expected {wanted})",
                files.short(&args.to_string())
            ),
        ));
    }
    Ok(verdict(checks))
}

pub async fn live_anthropic_nested_path() -> anyhow::Result<Verdict> {
    let Some(key) = anthropic_key() else {
        return Ok(Verdict::Skip("ANTHROPIC_API_KEY is not set".into()));
    };
    let files = Files::create("nested")?;
    let tool = FileTool::new(
        "read_document",
        "Reads one text document and returns its content.",
        json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "object",
                    "description": "The document to read.",
                    "properties": { "path": { "type": "string", "description": "Absolute file path" } },
                    "required": ["path"]
                }
            },
            "required": ["target"]
        }),
    );
    let prompt = format!(
        "Make these two tool calls one at a time, in this order:\n\
         1. read_document with target.path \"{notes}\"\n\
         2. read_document with target.path \"{outside}\"\n\
         Then report each tool result in one short line.",
        notes = files.notes,
        outside = files.outside,
    );
    let turn = run_file_agent(key, tool.clone(), &files, &prompt).await?;
    Ok(verdict(path_checks(&turn, &tool, &files)))
}

pub async fn live_anthropic_array_paths() -> anyhow::Result<Verdict> {
    let Some(key) = anthropic_key() else {
        return Ok(Verdict::Skip("ANTHROPIC_API_KEY is not set".into()));
    };
    let files = Files::create("array")?;
    let tool = FileTool::new(
        "read_documents",
        "Reads several text documents and returns their contents.",
        json!({
            "type": "object",
            "properties": {
                "documents": {
                    "type": "array",
                    "description": "The documents to read.",
                    "items": {
                        "type": "object",
                        "properties": { "path": { "type": "string", "description": "Absolute file path" } },
                        "required": ["path"]
                    }
                }
            },
            "required": ["documents"]
        }),
    );
    let prompt = format!(
        "Make these two tool calls one at a time, in this order:\n\
         1. read_documents with documents [{{\"path\": \"{notes}\"}}, {{\"path\": \"{plan}\"}}]\n\
         2. read_documents with documents [{{\"path\": \"{notes}\"}}, {{\"path\": \"{outside}\"}}]\n\
         Then report each tool result in one short line.",
        notes = files.notes,
        plan = files.plan,
        outside = files.outside,
    );
    let turn = run_file_agent(key, tool.clone(), &files, &prompt).await?;
    let mut checks = path_checks(&turn, &tool, &files);
    checks.push((
        tool.opened().contains(&files.plan),
        "the second path inside the root, held in the same array, was opened".into(),
    ));
    Ok(verdict(checks))
}
