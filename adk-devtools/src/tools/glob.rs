//! `glob` — list workspace files matching a glob pattern.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};
use walkdir::WalkDir;

use crate::error::DevToolError;
use crate::tools::read::require_str;
use crate::workspace::Workspace;

const MAX_RESULTS: usize = 1000;

/// Lists files matching a glob pattern (e.g. `src/**/*.rs`), relative to the
/// workspace root or an optional sub-directory.
pub struct GlobTool {
    workspace: Workspace,
}

impl GlobTool {
    /// Create a `glob` tool bound to `workspace`.
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "List files matching a glob pattern (e.g. 'src/**/*.rs'). Returns paths \
         relative to the workspace root. The pattern is relative to the workspace \
         and may not contain '..' or an absolute path."
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
                "pattern": { "type": "string", "description": "Glob pattern, e.g. '**/*.toml'." },
                "path": { "type": "string", "description": "Optional sub-directory to search within." }
            },
            "required": ["pattern"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let pattern = require_str(&args, "pattern")?;
        // `glob` follows `..` and absolute prefixes literally, so either would list
        // directories outside the workspace.
        let escapes = Path::new(&pattern).components().any(|component| match component {
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => true,
            Component::CurDir | Component::Normal(_) => false,
        });
        if escapes {
            return Err(DevToolError::PathEscape(pattern).into());
        }
        let base = match args.get("path").and_then(Value::as_str) {
            Some(sub) => self.workspace.resolve(sub)?,
            None => self.workspace.root().to_path_buf(),
        };

        let matcher = glob::Pattern::new(&pattern)
            .map_err(|e| DevToolError::Other(format!("invalid glob pattern: {e}")))?;
        // `*` and `?` stay within one component, as `glob::glob` treats them, so `src/*.rs`
        // cannot reach `src/a/b.rs`; `**` still spans directories.
        let options = glob::MatchOptions { require_literal_separator: true, ..Default::default() };

        // Walk from the base and match the path relative to it. Expanding the base through
        // `glob::glob` broke on Windows, where a canonical root carries the `\\?\` verbatim
        // prefix and `?` is a wildcard. The pattern's literal leading components narrow the
        // walk, so `src/**/*.rs` never visits `target/`.
        let literal: PathBuf = Path::new(&pattern)
            .components()
            .take_while(|component| match component {
                Component::Normal(part) => !part.to_string_lossy().contains(['*', '?', '[']),
                Component::Prefix(_)
                | Component::RootDir
                | Component::CurDir
                | Component::ParentDir => false,
            })
            .collect();
        let start = base.join(literal);

        let mut matches = Vec::new();
        let mut truncated = false;
        if start.exists() {
            // Symlinked directories are listed but not descended: one that points outside
            // the workspace would otherwise be walked before containment is checked.
            for entry in WalkDir::new(&start).sort_by_file_name().into_iter().flatten() {
                let path = entry.path();
                let Ok(relative) = path.strip_prefix(&base) else { continue };
                if relative.as_os_str().is_empty() || !matcher.matches_path_with(relative, options)
                {
                    continue;
                }
                // A symlink can point anywhere.
                let contained = std::fs::canonicalize(path)
                    .is_ok_and(|canonical| canonical.starts_with(self.workspace.root()));
                if !contained {
                    continue;
                }
                if matches.len() >= MAX_RESULTS {
                    truncated = true;
                    break;
                }
                matches.push(self.workspace.display(path));
            }
        }

        Ok(json!({
            "pattern": pattern,
            "matches": matches,
            "count": matches.len(),
            "truncated": truncated,
        }))
    }
}
