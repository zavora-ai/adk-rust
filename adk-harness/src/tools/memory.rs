//! `memory_read`, `memory_write` and `memory_delete` (spec 3.1 §7.15).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use adk_core::{AdkError, ErrorCategory, ErrorComponent, Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::audit::HarnessAudit;
use crate::memory::{FileMemory, MemoryChange, MemoryConfig};
use crate::policy::has_harness_layer;

/// Opens the memory of the user a call runs for, one [`FileMemory`] per (app, user).
#[derive(Debug)]
pub(crate) struct MemoryResolver {
    config: MemoryConfig,
    workspace_root: PathBuf,
    open: Mutex<HashMap<(String, String), FileMemory>>,
}

impl MemoryResolver {
    pub(crate) fn new(config: MemoryConfig, workspace_root: PathBuf) -> Arc<Self> {
        Arc::new(Self { config, workspace_root, open: Mutex::new(HashMap::new()) })
    }

    /// The memory of `user` of `app`.
    pub(crate) fn memory(&self, app: &str, user: &str) -> Result<FileMemory> {
        let key = (app.to_string(), user.to_string());
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(memory) = open.get(&key) {
            return Ok(memory.clone());
        }
        let memory = FileMemory::for_user(&self.config, app, user, &self.workspace_root)?;
        if open.len() > 1024 {
            open.clear();
        }
        open.insert(key, memory.clone());
        Ok(memory)
    }

    /// `instruction` with the memory index of the run's user appended, inside its
    /// delimited block (spec 3.1 §7.7 item 6). An unreadable memory adds nothing.
    pub(crate) async fn append_index(
        &self,
        instruction: String,
        ctx: &dyn adk_core::ReadonlyContext,
    ) -> String {
        let index = match self.memory(ctx.app_name(), ctx.user_id()) {
            Ok(memory) => memory.index().await,
            Err(error) => Err(error),
        };
        match index {
            Ok(index) => format!("{instruction}\n\n{}", crate::memory::instruction_block(&index)),
            Err(error) => {
                tracing::warn!(error = %error, "memory index unavailable; the instruction carries none");
                instruction
            }
        }
    }
}

fn required<'a>(args: &'a Value, key: &str, tool: &str) -> Result<&'a str> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| {
        AdkError::new(
            ErrorComponent::Tool,
            ErrorCategory::InvalidInput,
            "harness.memory.invalid_args",
            format!("{tool} needs a string '{key}'"),
        )
    })
}

/// Refuses a memory write outside a harness run: the write is non-waivable, so it must
/// have passed the harness policy (spec 3.1 §9.2 I17, I33).
fn require_harness_run(ctx: &dyn ToolContext, tool: &str) -> Result<()> {
    let governed = ctx
        .run_config()
        .and_then(|config| config.tool_policy.as_deref())
        .is_some_and(has_harness_layer);
    if governed {
        return Ok(());
    }
    Err(AdkError::new(
        ErrorComponent::Tool,
        ErrorCategory::Forbidden,
        "harness.memory.ungoverned",
        format!("{tool} runs only inside a harness run, under the harness policy"),
    ))
}

/// Records `harness.memory.written` before the change is made (spec 3.1 §9.3).
async fn record_change(ctx: &dyn ToolContext, change: &MemoryChange) -> Result<()> {
    let Some(audit) = HarnessAudit::for_tool(ctx) else {
        return Ok(());
    };
    audit
        .record(
            "harness.memory.written",
            json!({
                "logicalPath": change.path,
                "beforeSha256": change.before_sha256,
                "afterSha256": change.after_sha256,
                "bytes": change.bytes,
                "functionCallId": ctx.function_call_id(),
            }),
        )
        .await
}

/// `memory_read`: the memory index, or one topic (spec 3.1 §7.15; class `Read`).
///
/// # Example
///
/// ```rust,ignore
/// // memory_read {}                      → the index
/// // memory_read { "path": "build.md" }  → the topic's content
/// ```
pub struct MemoryReadTool {
    resolver: Arc<MemoryResolver>,
}

impl MemoryReadTool {
    pub(crate) fn new(resolver: Arc<MemoryResolver>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl Tool for MemoryReadTool {
    fn name(&self) -> &str {
        "memory_read"
    }

    fn description(&self) -> &str {
        "Read your memory from earlier sessions: without a path, the index of topics with \
         their summaries; with a path, that topic's content."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Topic path from the index, such as 'build.md'. Omit it for the index." }
            }
        }))
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let memory = self.resolver.memory(ctx.app_name(), ctx.user_id())?;
        match args.get("path").and_then(Value::as_str) {
            Some(path) => Ok(json!({ "path": path, "content": memory.read(path).await? })),
            None => Ok(json!({ "index": memory.index().await? })),
        }
    }
}

/// `memory_write`: writes one topic (spec 3.1 §7.15; class `MemoryWrite`, a non-waivable
/// approval in Ask and Auto, denied in ReadOnly and Plan).
///
/// # Example
///
/// ```rust,ignore
/// // memory_write { "path": "build.md", "summary": "how to build", "content": "cargo build" }
/// ```
pub struct MemoryWriteTool {
    resolver: Arc<MemoryResolver>,
}

impl MemoryWriteTool {
    pub(crate) fn new(resolver: Arc<MemoryResolver>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl Tool for MemoryWriteTool {
    fn name(&self) -> &str {
        "memory_write"
    }

    fn description(&self) -> &str {
        "Save something worth remembering in later sessions with this user in this \
         workspace, as one topic file with a one-line summary. Replaces the topic if it \
         exists. The user approves every write."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Topic path, such as 'build.md' or 'ci/flaky-tests.md'." },
                "summary": { "type": "string", "description": "One line that says what the topic holds; it appears in the index." },
                "content": { "type": "string", "description": "The topic's full content, in Markdown." }
            },
            "required": ["path", "summary", "content"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        require_harness_run(ctx.as_ref(), "memory_write")?;
        let path = required(&args, "path", "memory_write")?;
        let summary = required(&args, "summary", "memory_write")?;
        let content = required(&args, "content", "memory_write")?;
        let memory = self.resolver.memory(ctx.app_name(), ctx.user_id())?;
        let planned = memory.plan_write(path, content, summary).await?;
        record_change(ctx.as_ref(), &planned).await?;
        let change = memory.write(path, content, summary).await?;
        Ok(
            json!({ "path": change.path, "bytes": change.bytes, "created": change.before_sha256.is_none() }),
        )
    }
}

/// `memory_delete`: deletes one topic (spec 3.1 §7.15; class `MemoryWrite`).
///
/// # Example
///
/// ```rust,ignore
/// // memory_delete { "path": "build.md" }
/// ```
pub struct MemoryDeleteTool {
    resolver: Arc<MemoryResolver>,
}

impl MemoryDeleteTool {
    pub(crate) fn new(resolver: Arc<MemoryResolver>) -> Self {
        Self { resolver }
    }
}

#[async_trait]
impl Tool for MemoryDeleteTool {
    fn name(&self) -> &str {
        "memory_delete"
    }

    fn description(&self) -> &str {
        "Delete a memory topic that is wrong or no longer useful. The user approves every delete."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Topic path from the index." }
            },
            "required": ["path"]
        }))
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::Idempotent
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        require_harness_run(ctx.as_ref(), "memory_delete")?;
        let path = required(&args, "path", "memory_delete")?;
        let memory = self.resolver.memory(ctx.app_name(), ctx.user_id())?;
        let planned = memory.plan_delete(path).await?;
        if planned.before_sha256.is_some() {
            record_change(ctx.as_ref(), &planned).await?;
        }
        let change = memory.delete(path).await?;
        Ok(json!({ "path": change.path, "deleted": change.before_sha256.is_some() }))
    }
}
