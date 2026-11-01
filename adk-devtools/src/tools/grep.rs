//! `grep` — regex content search across workspace files.

use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};
use walkdir::WalkDir;

use crate::error::DevToolError;
use crate::tools::read::require_str;
use crate::workspace::Workspace;

const MAX_MATCHES: usize = 500;
const MAX_FILE_BYTES: u64 = 5 * 1_048_576;
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".venv", "dist", "build"];

/// Searches file contents for a regular expression, returning `path:line: text`
/// matches. Skips common build/VCS directories, binary/oversized files and files
/// matching the workspace's exclusion patterns ([`Workspace::exclude_globs`]).
pub struct GrepTool {
    workspace: Workspace,
}

impl GrepTool {
    /// Create a `grep` tool bound to `workspace`.
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    /// Searches the workspace on disk.
    fn egress(&self, _args: &serde_json::Value) -> adk_core::egress::ToolEgress {
        adk_core::egress::ToolEgress::None
    }

    fn description(&self) -> &str {
        "Search file contents for a regular expression. Returns matching lines as \
         'path:line: text'. Optionally restrict to a sub-path and/or a glob over file names."
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression to search for." },
                "path": { "type": "string", "description": "Optional sub-directory to search within." },
                "glob": { "type": "string", "description": "Optional glob over file names, e.g. '*.rs'." },
                "case_insensitive": { "type": "boolean", "description": "Case-insensitive match (default false)." }
            },
            "required": ["pattern"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let pattern = require_str(&args, "pattern")?;
        let case_insensitive =
            args.get("case_insensitive").and_then(Value::as_bool).unwrap_or(false);
        let name_glob = args
            .get("glob")
            .and_then(Value::as_str)
            .map(glob::Pattern::new)
            .transpose()
            .map_err(|e| DevToolError::Other(format!("invalid glob: {e}")))?;

        let re = regex::RegexBuilder::new(&pattern)
            .case_insensitive(case_insensitive)
            .build()
            .map_err(|e| DevToolError::Other(format!("invalid regex: {e}")))?;

        let base = match args.get("path").and_then(Value::as_str) {
            Some(sub) => self.workspace.resolve(sub)?,
            None => self.workspace.root().to_path_buf(),
        };

        let (matches, truncated) = search(&base, &re, name_glob.as_ref(), &self.workspace);

        Ok(json!({
            "pattern": pattern,
            "matches": matches,
            "count": matches.len(),
            "truncated": truncated,
        }))
    }
}

fn search(
    base: &std::path::Path,
    re: &Regex,
    name_glob: Option<&glob::Pattern>,
    ws: &Workspace,
) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let walker = WalkDir::new(base).into_iter().filter_entry(|e| {
        !e.file_name().to_str().map(|n| SKIP_DIRS.contains(&n)).unwrap_or(false)
            && !ws.is_excluded(e.path())
    });

    for entry in walker.filter_map(std::result::Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if let Some(g) = name_glob {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !g.matches(name) {
                continue;
            }
        }
        if entry.metadata().map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(true) {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(path) else {
            continue; // skip binary / non-utf8
        };
        let display = ws.display(path);
        for (idx, line) in contents.lines().enumerate() {
            if re.is_match(line) {
                out.push(format!("{}:{}: {}", display, idx + 1, line.trim_end()));
                if out.len() >= MAX_MATCHES {
                    return (out, true);
                }
            }
        }
    }
    (out, false)
}
