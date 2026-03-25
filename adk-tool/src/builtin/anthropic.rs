use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

fn invalid_input(message: impl Into<String>) -> AdkError {
    AdkError::tool(message.into())
}

fn resolve_workspace_path(path: &str) -> std::result::Result<PathBuf, String> {
    let raw = Path::new(path);
    let resolved = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("failed to resolve current directory: {error}"))?
            .join(raw)
    };
    Ok(resolved)
}

async fn render_view(path: &str, view_range: Option<(u32, u32)>) -> Result<String> {
    if let Some((start, end)) = view_range
        && (start == 0 || end == 0 || start > end)
    {
        return Err(invalid_input("view_range must use positive 1-based line numbers"));
    }

    let resolved = resolve_workspace_path(path).map_err(AdkError::tool)?;
    let metadata = tokio::fs::metadata(&resolved)
        .await
        .map_err(|error| AdkError::tool(format!("failed to inspect '{path}': {error}")))?;

    if metadata.is_dir() {
        let mut entries = tokio::fs::read_dir(&resolved).await.map_err(|error| {
            AdkError::tool(format!("failed to read directory '{path}': {error}"))
        })?;
        let mut listing = String::new();
        while let Some(entry) = entries.next_entry().await.map_err(|error| {
            AdkError::tool(format!("failed to read directory '{path}': {error}"))
        })? {
            let name = entry.file_name();
            listing.push_str(&name.to_string_lossy());
            listing.push('\n');
        }
        return Ok(listing);
    }

    let content = tokio::fs::read_to_string(&resolved)
        .await
        .map_err(|error| AdkError::tool(format!("failed to read '{path}': {error}")))?;

    let lines: Vec<&str> = content.split_terminator('\n').collect();
    let visible = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let line_no = index as u32 + 1;
            view_range
                .map(|(start, end)| (start..=end).contains(&line_no))
                .unwrap_or(true)
                .then_some(*line)
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(format!("{visible}\n"))
}

async fn create_file(path: &str, file_text: &str) -> Result<String> {
    let resolved = resolve_workspace_path(path).map_err(AdkError::tool)?;
    if let Some(parent) = resolved.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            AdkError::tool(format!("failed to create parent directories for '{path}': {error}"))
        })?;
    }

    let mut file =
        std::fs::OpenOptions::new().create_new(true).write(true).open(&resolved).map_err(
            |error| match error.kind() {
                ErrorKind::AlreadyExists => AdkError::tool(format!("file '{path}' already exists")),
                _ => AdkError::tool(format!("failed to create '{path}': {error}")),
            },
        )?;
    file.write_all(file_text.as_bytes())
        .map_err(|error| AdkError::tool(format!("failed to write '{path}': {error}")))?;
    Ok("success".to_string())
}

async fn str_replace(path: &str, old_str: &str, new_str: &str) -> Result<String> {
    let resolved = resolve_workspace_path(path).map_err(AdkError::tool)?;
    let content = tokio::fs::read_to_string(&resolved)
        .await
        .map_err(|error| AdkError::tool(format!("failed to read '{path}': {error}")))?;

    let matches = content.matches(old_str).count();
    match matches {
        0 => Err(invalid_input(format!("old_str not found in '{path}'"))),
        1 => {
            let updated = content.replacen(old_str, new_str, 1);
            tokio::fs::write(&resolved, updated)
                .await
                .map_err(|error| AdkError::tool(format!("failed to update '{path}': {error}")))?;
            Ok("success".to_string())
        }
        _ => Err(invalid_input(format!(
            "old_str appears multiple times in '{path}'; use a more specific match"
        ))),
    }
}

async fn insert_text(path: &str, insert_line: u32, insert_text: &str) -> Result<String> {
    if insert_line == 0 {
        return Err(invalid_input("insert_line must be >= 1"));
    }

    let resolved = resolve_workspace_path(path).map_err(AdkError::tool)?;
    let content = tokio::fs::read_to_string(&resolved)
        .await
        .map_err(|error| AdkError::tool(format!("failed to read '{path}': {error}")))?;
    let mut lines = content.split_terminator('\n').map(str::to_string).collect::<Vec<_>>();

    let insert_index = insert_line as usize - 1;
    if insert_index > lines.len() {
        return Err(invalid_input(format!(
            "insert_line {insert_line} is out of range for '{path}'"
        )));
    }

    lines.insert(insert_index, insert_text.to_string());
    let mut updated = lines.join("\n");
    updated.push('\n');
    tokio::fs::write(&resolved, updated)
        .await
        .map_err(|error| AdkError::tool(format!("failed to update '{path}': {error}")))?;
    Ok("success".to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BashVersion {
    V20241022,
    V20250124,
}

impl BashVersion {
    fn type_name(self) -> &'static str {
        match self {
            Self::V20241022 => "bash_20241022",
            Self::V20250124 => "bash_20250124",
        }
    }
}

#[derive(Debug, Clone)]
struct AnthropicBashTool {
    version: BashVersion,
}

impl AnthropicBashTool {
    const fn new(version: BashVersion) -> Self {
        Self { version }
    }

    fn declaration_json(&self) -> Value {
        json!({
            "type": self.version.type_name(),
            "name": "bash",
        })
    }
}

#[derive(Debug, Deserialize)]
struct BashArgs {
    command: String,
    #[serde(default)]
    restart: bool,
}

#[async_trait]
impl Tool for AnthropicBashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Executes shell commands for Anthropic's native bash tool."
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-anthropic-tool": self.declaration_json(),
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let args: BashArgs = serde_json::from_value(args)
            .map_err(|error| AdkError::tool(format!("invalid bash arguments: {error}")))?;
        let output = tokio::process::Command::new("sh")
            .arg("-lc")
            .arg(&args.command)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|error| AdkError::tool(format!("failed to execute bash command: {error}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);
        let restart_note =
            if args.restart { "bash session restart requested before execution\n" } else { "" };

        Ok(Value::String(format!("{restart_note}{stdout}{stderr}\nexit_code: {exit_code}\n")))
    }
}

/// Anthropic native bash tool declaration for the `bash_20241022` version.
#[derive(Debug, Clone, Default)]
pub struct AnthropicBashTool20241022;

impl AnthropicBashTool20241022 {
    /// Create a new `bash_20241022` tool.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for AnthropicBashTool20241022 {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Executes shell commands for Anthropic's native bash tool."
    }

    fn declaration(&self) -> Value {
        AnthropicBashTool::new(BashVersion::V20241022).declaration()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        AnthropicBashTool::new(BashVersion::V20241022).execute(ctx, args).await
    }
}

/// Anthropic native bash tool declaration for the `bash_20250124` version.
#[derive(Debug, Clone, Default)]
pub struct AnthropicBashTool20250124;

impl AnthropicBashTool20250124 {
    /// Create a new `bash_20250124` tool.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for AnthropicBashTool20250124 {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Executes shell commands for Anthropic's native bash tool."
    }

    fn declaration(&self) -> Value {
        AnthropicBashTool::new(BashVersion::V20250124).declaration()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        AnthropicBashTool::new(BashVersion::V20250124).execute(ctx, args).await
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextEditorVersion {
    V20250124,
    V20250429,
    V20250728,
}

impl TextEditorVersion {
    fn type_name(self) -> &'static str {
        match self {
            Self::V20250124 => "text_editor_20250124",
            Self::V20250429 => "text_editor_20250429",
            Self::V20250728 => "text_editor_20250728",
        }
    }

    fn tool_name(self) -> &'static str {
        match self {
            Self::V20250124 => "str_replace_editor",
            Self::V20250429 | Self::V20250728 => "str_replace_based_edit_tool",
        }
    }
}

#[derive(Debug, Clone)]
struct AnthropicTextEditorTool {
    version: TextEditorVersion,
    max_characters: Option<i32>,
}

impl AnthropicTextEditorTool {
    const fn new(version: TextEditorVersion, max_characters: Option<i32>) -> Self {
        Self { version, max_characters }
    }

    fn declaration_json(&self) -> Value {
        json!({
            "type": self.version.type_name(),
            "name": self.version.tool_name(),
            "max_characters": self.max_characters,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ViewArgs {
    path: String,
    view_range: Option<(u32, u32)>,
}

#[derive(Debug, Deserialize)]
struct StrReplaceArgs {
    path: String,
    old_str: String,
    new_str: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InsertArgs {
    path: String,
    insert_line: u32,
    insert_text: Option<String>,
    new_str: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateArgs {
    path: String,
    file_text: String,
}

#[derive(Debug, Deserialize)]
struct TextEditorCommand {
    command: String,
}

#[async_trait]
impl Tool for AnthropicTextEditorTool {
    fn name(&self) -> &str {
        self.version.tool_name()
    }

    fn description(&self) -> &str {
        "Executes Anthropic's native text editor commands against local files."
    }

    fn declaration(&self) -> Value {
        json!({
            "name": self.name(),
            "description": self.description(),
            "x-adk-anthropic-tool": self.declaration_json(),
        })
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let command: TextEditorCommand = serde_json::from_value(args.clone())
            .map_err(|error| AdkError::tool(format!("invalid text editor arguments: {error}")))?;

        let rendered = match command.command.as_str() {
            "view" => {
                let args: ViewArgs = serde_json::from_value(args).map_err(|error| {
                    AdkError::tool(format!("invalid text editor view arguments: {error}"))
                })?;
                render_view(&args.path, args.view_range).await?
            }
            "str_replace" => {
                let args: StrReplaceArgs = serde_json::from_value(args).map_err(|error| {
                    AdkError::tool(format!("invalid text editor replace arguments: {error}"))
                })?;
                str_replace(&args.path, &args.old_str, args.new_str.as_deref().unwrap_or(""))
                    .await?
            }
            "insert" => {
                let args: InsertArgs = serde_json::from_value(args).map_err(|error| {
                    AdkError::tool(format!("invalid text editor insert arguments: {error}"))
                })?;
                let payload = args.insert_text.or(args.new_str).ok_or_else(|| {
                    invalid_input("text editor insert requires insert_text or new_str")
                })?;
                insert_text(&args.path, args.insert_line, &payload).await?
            }
            "create" => {
                let args: CreateArgs = serde_json::from_value(args).map_err(|error| {
                    AdkError::tool(format!("invalid text editor create arguments: {error}"))
                })?;
                create_file(&args.path, &args.file_text).await?
            }
            other => {
                return Err(invalid_input(format!("unsupported text editor command '{other}'")));
            }
        };

        Ok(Value::String(rendered))
    }
}

/// Anthropic native text editor declaration for `text_editor_20250124`.
#[derive(Debug, Clone, Default)]
pub struct AnthropicTextEditorTool20250124;

impl AnthropicTextEditorTool20250124 {
    /// Create a new `text_editor_20250124` tool.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for AnthropicTextEditorTool20250124 {
    fn name(&self) -> &str {
        "str_replace_editor"
    }

    fn description(&self) -> &str {
        "Executes Anthropic's native text editor commands against local files."
    }

    fn declaration(&self) -> Value {
        AnthropicTextEditorTool::new(TextEditorVersion::V20250124, None).declaration()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        AnthropicTextEditorTool::new(TextEditorVersion::V20250124, None).execute(ctx, args).await
    }
}

/// Anthropic native text editor declaration for `text_editor_20250429`.
#[derive(Debug, Clone, Default)]
pub struct AnthropicTextEditorTool20250429 {
    max_characters: Option<i32>,
}

impl AnthropicTextEditorTool20250429 {
    /// Create a new `text_editor_20250429` tool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Limit the number of characters returned when viewing a file.
    pub fn with_max_characters(mut self, max_characters: i32) -> Self {
        self.max_characters = Some(max_characters);
        self
    }
}

#[async_trait]
impl Tool for AnthropicTextEditorTool20250429 {
    fn name(&self) -> &str {
        "str_replace_based_edit_tool"
    }

    fn description(&self) -> &str {
        "Executes Anthropic's native text editor commands against local files."
    }

    fn declaration(&self) -> Value {
        AnthropicTextEditorTool::new(TextEditorVersion::V20250429, self.max_characters)
            .declaration()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        AnthropicTextEditorTool::new(TextEditorVersion::V20250429, self.max_characters)
            .execute(ctx, args)
            .await
    }
}

/// Anthropic native text editor declaration for `text_editor_20250728`.
#[derive(Debug, Clone, Default)]
pub struct AnthropicTextEditorTool20250728 {
    max_characters: Option<i32>,
}

impl AnthropicTextEditorTool20250728 {
    /// Create a new `text_editor_20250728` tool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Limit the number of characters returned when viewing a file.
    pub fn with_max_characters(mut self, max_characters: i32) -> Self {
        self.max_characters = Some(max_characters);
        self
    }
}

#[async_trait]
impl Tool for AnthropicTextEditorTool20250728 {
    fn name(&self) -> &str {
        "str_replace_based_edit_tool"
    }

    fn description(&self) -> &str {
        "Executes Anthropic's native text editor commands against local files."
    }

    fn declaration(&self) -> Value {
        AnthropicTextEditorTool::new(TextEditorVersion::V20250728, self.max_characters)
            .declaration()
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        AnthropicTextEditorTool::new(TextEditorVersion::V20250728, self.max_characters)
            .execute(ctx, args)
            .await
    }
}
