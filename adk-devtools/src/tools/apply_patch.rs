//! `apply_patch` — a multi-file patch, checked whole and written all or nothing
//! (spec 3.1 §7.26, §10 D8).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext, ToolEffect};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::DevToolError;
use crate::patch::{PatchOp, apply_hunks, parse_patch};
use crate::read_ledger::sha256_hex;
use crate::tools::read::require_str;
use crate::workspace::{Precondition, Workspace};

/// Applies a `*** Begin Patch` envelope to several files at once (spec 3.1 §7.26).
///
/// The whole patch is parsed and every hunk located before anything is written. Each
/// file then goes to a temporary sibling, opened without following a symlink, and is
/// renamed into place; on any failure the files already written are restored from their
/// in-memory originals, so the call writes every file or none (§9.2 I67). A file's
/// precondition is its `expect_sha256` entry when the call gives one, else this session and
/// agent's read record when one exists, and a file that differs is answered
/// `devtools.stale`. Each file keeps its line endings. The report lists every file with
/// its digests before and after:
///
/// ```json
/// { "files": [{ "path": "src/lib.rs", "op": "update", "beforeSha256": "…", "afterSha256": "…" }] }
/// ```
///
/// # Example
///
/// ```rust
/// use adk_core::{Tool, ToolEffect};
/// use adk_devtools::{ApplyPatchTool, Workspace};
///
/// let tool = ApplyPatchTool::new(Workspace::new("./my-repo"));
/// assert_eq!(tool.name(), "apply_patch");
/// assert_eq!(tool.effect(), ToolEffect::NonIdempotent);
/// ```
pub struct ApplyPatchTool {
    workspace: Workspace,
}

impl ApplyPatchTool {
    /// Create an `apply_patch` tool bound to `workspace`.
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

/// One file of a checked patch, with everything needed to write it and to undo it.
struct Planned {
    op: &'static str,
    /// Where the content goes: the file, or a move's destination.
    path: PathBuf,
    target: PathBuf,
    /// A move's source.
    from: Option<(PathBuf, PathBuf)>,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}

/// A step already written, and how to take it back: restore `target` to `original`, or
/// remove it when it did not exist; `now` is what it holds now.
struct Undo {
    target: PathBuf,
    original: Option<Vec<u8>>,
    now: Option<Vec<u8>>,
}

impl ApplyPatchTool {
    /// Checks an existing file against its precondition: the call's `expect_sha256`, else
    /// the read record. Without either, the write later binds the content just read, so
    /// nothing changes between check and write.
    async fn check_existing(
        &self,
        ctx: &dyn ToolContext,
        expects: &HashMap<String, Precondition>,
        resolved: &std::path::Path,
        current: &[u8],
    ) -> std::result::Result<(), DevToolError> {
        let key = self.workspace.record_path(resolved);
        let expected = match expects.get(&key) {
            Some(expected) => expected.clone(),
            None => match self.workspace.read_digest(ctx, resolved).await? {
                Some(digest) => Precondition::Sha256(digest),
                None => Precondition::Sha256(sha256_hex(current)),
            },
        };
        expected.check(Some(current)).map_err(|error| DevToolError::Stale {
            path: self.workspace.display(resolved),
            detail: error.get_ref().map(ToString::to_string).unwrap_or_else(|| error.to_string()),
        })
    }

    /// Checks the whole patch and plans every file, writing nothing.
    async fn plan(
        &self,
        ctx: &dyn ToolContext,
        patch: &crate::patch::Patch,
        expects: &HashMap<String, Precondition>,
    ) -> Result<Vec<Planned>> {
        let mut seen = HashSet::new();
        for path in patch.paths() {
            let (resolved, _) = self.workspace.resolve_target(path)?;
            if !seen.insert(self.workspace.record_path(&resolved)) {
                return Err(DevToolError::Other(format!(
                    "'{path}' appears twice in the patch; put every change to a file in one \
                     'Update File' section"
                ))
                .into());
            }
        }
        let absent = |resolved: &std::path::Path| -> std::result::Result<(), DevToolError> {
            let key = self.workspace.record_path(resolved);
            match expects.get(&key) {
                Some(Precondition::Sha256(_)) => Err(DevToolError::Stale {
                    path: self.workspace.display(resolved),
                    detail: "the call expected a file there, and there is none".to_string(),
                }),
                _ => Ok(()),
            }
        };
        let mut planned = Vec::new();
        for op in &patch.ops {
            match op {
                PatchOp::Add { path, contents } => {
                    let (resolved, target) = self.workspace.resolve_target(path)?;
                    if self.exists(&target).await? {
                        return Err(DevToolError::Other(format!(
                            "'{path}' already exists; change it with '*** Update File: {path}'"
                        ))
                        .into());
                    }
                    absent(&resolved)?;
                    planned.push(Planned {
                        op: "add",
                        path: resolved,
                        target,
                        from: None,
                        before: None,
                        after: Some(contents.clone().into_bytes()),
                    });
                }
                PatchOp::Delete { path } => {
                    let (resolved, target) = self.workspace.resolve_target(path)?;
                    let current = self.workspace.read_contained_bytes(&target).await?;
                    self.check_existing(ctx, expects, &resolved, &current).await?;
                    planned.push(Planned {
                        op: "delete",
                        path: resolved,
                        target,
                        from: None,
                        before: Some(current),
                        after: None,
                    });
                }
                PatchOp::Update { path, move_to, hunks } => {
                    let (resolved, target) = self.workspace.resolve_target(path)?;
                    // A failing hunk reports file lines, so a secret file is not patched.
                    if self.workspace.is_excluded(&resolved) || self.workspace.is_excluded(&target)
                    {
                        return Err(
                            DevToolError::Excluded(self.workspace.display(&resolved)).into()
                        );
                    }
                    let current = self.workspace.read_contained(&target).await?;
                    self.check_existing(ctx, expects, &resolved, current.as_bytes()).await?;
                    let display = self.workspace.display(&resolved);
                    let updated = if hunks.is_empty() {
                        current.clone()
                    } else {
                        apply_hunks(&display, &current, hunks)
                            .map_err(|error| DevToolError::Other(error.0))?
                    };
                    let (op, path, target, from) = match move_to {
                        Some(to) => {
                            let (to_resolved, to_target) = self.workspace.resolve_target(to)?;
                            if self.exists(&to_target).await? {
                                return Err(DevToolError::Other(format!(
                                    "'{to}' already exists; a move does not replace a file"
                                ))
                                .into());
                            }
                            absent(&to_resolved)?;
                            ("move", to_resolved, to_target, Some((resolved, target)))
                        }
                        None => ("update", resolved, target, None),
                    };
                    planned.push(Planned {
                        op,
                        path,
                        target,
                        from,
                        before: Some(current.into_bytes()),
                        after: Some(updated.into_bytes()),
                    });
                }
            }
        }
        Ok(planned)
    }

    async fn exists(&self, target: &std::path::Path) -> std::result::Result<bool, DevToolError> {
        match tokio::fs::symlink_metadata(target).await {
            Ok(_) => Ok(true),
            // Nothing exists beneath a file either.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Writes one planned file, pushing how to undo each step written.
    async fn write(
        &self,
        file: &Planned,
        undo: &mut Vec<Undo>,
    ) -> std::result::Result<(), DevToolError> {
        let ws = &self.workspace;
        match (&file.from, &file.before, &file.after) {
            (None, None, Some(after)) => {
                ws.replace_contained(&file.target, after, Precondition::Absent).await?;
                undo.push(Undo {
                    target: file.target.clone(),
                    original: None,
                    now: Some(after.clone()),
                });
            }
            (None, Some(before), None) => {
                ws.remove_contained(&file.target, Precondition::Sha256(sha256_hex(before))).await?;
                undo.push(Undo {
                    target: file.target.clone(),
                    original: Some(before.clone()),
                    now: None,
                });
            }
            (None, Some(before), Some(after)) => {
                ws.replace_contained(&file.target, after, Precondition::Sha256(sha256_hex(before)))
                    .await?;
                undo.push(Undo {
                    target: file.target.clone(),
                    original: Some(before.clone()),
                    now: Some(after.clone()),
                });
            }
            (Some((_, source)), Some(before), Some(after)) => {
                ws.replace_contained(&file.target, after, Precondition::Absent).await?;
                undo.push(Undo {
                    target: file.target.clone(),
                    original: None,
                    now: Some(after.clone()),
                });
                ws.remove_contained(source, Precondition::Sha256(sha256_hex(before))).await?;
                undo.push(Undo {
                    target: source.clone(),
                    original: Some(before.clone()),
                    now: None,
                });
            }
            _ => {
                return Err(DevToolError::Other("the patch planned an impossible step".into()));
            }
        }
        Ok(())
    }

    /// Takes back every written step, newest first; returns the files it could not restore.
    async fn restore(&self, undo: Vec<Undo>) -> Vec<String> {
        let mut failed = Vec::new();
        for step in undo.into_iter().rev() {
            let Undo { target, original, now } = step;
            let now = match &now {
                Some(bytes) => Precondition::Sha256(sha256_hex(bytes)),
                None => Precondition::Absent,
            };
            let restored = match &original {
                Some(bytes) => self.workspace.replace_contained(&target, bytes, now).await,
                None => self.workspace.remove_contained(&target, now).await,
            };
            if let Err(error) = restored {
                let path = self.workspace.display(&target);
                tracing::warn!(file.path = %path, error = %error, "apply_patch could not restore a file");
                failed.push(path);
            }
        }
        failed
    }
}

#[async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply a patch that adds, deletes, updates or moves several files at once, all or \
         nothing. The patch starts with '*** Begin Patch' and ends with '*** End Patch'; each \
         file is a section: '*** Add File: <path>' followed by lines starting with '+'; \
         '*** Delete File: <path>'; or '*** Update File: <path>', optionally '*** Move to: \
         <path>', then hunks. A hunk starts with '@@' (optionally followed by a line of the \
         file, such as a function signature, that precedes the change) and lists context lines \
         starting with ' ', removed lines starting with '-' and added lines starting with '+'. \
         Use no line numbers; give about three lines of context so each hunk matches exactly \
         one place."
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::NonIdempotent
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "description": "The patch text, from '*** Begin Patch' to '*** End Patch'." },
                "expect_sha256": {
                    "type": "object",
                    "description": "Optional: per path, the sha256 the file must have, or \"absent\" for a file that must not exist yet.",
                    "additionalProperties": { "type": "string" }
                }
            },
            "required": ["patch"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        if !self.workspace.is_writable() {
            return Err(DevToolError::ReadOnly("apply_patch").into());
        }
        let text = require_str(&args, "patch")?;
        let patch = parse_patch(&text).map_err(|error| DevToolError::Other(error.0))?;
        let mut expects = HashMap::new();
        if let Some(map) = args.get("expect_sha256").filter(|value| !value.is_null()) {
            let Some(map) = map.as_object() else {
                return Err(DevToolError::Other(
                    "expect_sha256 maps each path to a sha256 or \"absent\"".into(),
                )
                .into());
            };
            for (path, expect) in map {
                let Some(expect) = expect.as_str() else {
                    return Err(DevToolError::Other(format!(
                        "expect_sha256['{path}'] must be a string"
                    ))
                    .into());
                };
                let (resolved, _) = self.workspace.resolve_target(path)?;
                expects.insert(
                    self.workspace.record_path(&resolved),
                    Precondition::from_expect(expect)?,
                );
            }
        }

        let planned = self.plan(ctx.as_ref(), &patch, &expects).await?;
        let mut undo = Vec::new();
        for file in &planned {
            if let Err(error) = self.write(file, &mut undo).await {
                let failed = self.restore(undo).await;
                if failed.is_empty() {
                    return Err(error.into());
                }
                return Err(DevToolError::Other(format!(
                    "{error}; the patch was not applied, and these files could not be restored \
                     to their content before it: {}",
                    failed.join(", ")
                ))
                .into());
            }
        }

        let mut files = Vec::new();
        for file in &planned {
            let before = file.before.as_deref().map(sha256_hex);
            let after = file.after.as_deref().map(sha256_hex);
            match &after {
                Some(digest) => {
                    self.workspace.record_digest(ctx.as_ref(), &file.path, digest).await
                }
                None => self.workspace.forget_read(ctx.as_ref(), &file.path).await,
            }
            let mut entry = json!({
                "path": self.workspace.display(&file.path),
                "op": file.op,
                "beforeSha256": before,
                "afterSha256": after,
            });
            if let Some((from, _)) = &file.from {
                self.workspace.forget_read(ctx.as_ref(), from).await;
                entry["from"] = json!(self.workspace.display(from));
            }
            files.push(entry);
        }
        let summary = files
            .iter()
            .map(|file| {
                format!(
                    "{} {}",
                    file["op"].as_str().unwrap_or(""),
                    file["path"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        Ok(json!({
            "files": files,
            "message": format!("Applied the patch: {summary}"),
        }))
    }
}
