//! `edit_file` — exact-string replacement within a file.

use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::DevToolError;
use crate::read_ledger::sha256_hex;
use crate::tools::read::require_str;
use crate::workspace::{Precondition, Workspace};

/// Replaces exact substrings in a file: one `old_string`/`new_string` pair, or several as
/// `edits`, applied in order and written together or not at all (spec 3.1 §7.26).
///
/// The file must be unchanged since this session and agent last read or wrote it: the
/// read record's sha256 is the edit's precondition, or `expect_sha256` when the call
/// names one, and a file whose content differs is answered `devtools.stale` without being
/// written (§9.2 I66). Unless `replace_all`, each target string must occur exactly once.
///
/// On Unix the file is read and written without following a symlink on any component
/// of the target the containment check resolved, and a file with more than one hard
/// link is refused (spec 3.1 §10 D4).
///
/// # Example
///
/// ```rust
/// use adk_core::Tool;
/// use adk_devtools::{EditFileTool, Workspace};
///
/// let tool = EditFileTool::new(Workspace::new("./my-repo"));
/// let schema = tool.parameters_schema().unwrap();
/// assert!(schema["properties"]["edits"].is_object());
/// assert!(schema["properties"]["expect_sha256"].is_object());
/// ```
pub struct EditFileTool {
    workspace: Workspace,
}

impl EditFileTool {
    /// Create an `edit_file` tool bound to `workspace`.
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

/// One replacement of an `edit_file` call.
struct Replacement {
    old_string: String,
    new_string: String,
    replace_all: bool,
}

/// The replacements of a call: `edits`, or the single top-level pair.
fn replacements(args: &Value) -> Result<Vec<Replacement>> {
    let invalid = |message: String| -> adk_core::AdkError { DevToolError::Other(message).into() };
    let Some(edits) = args.get("edits").filter(|edits| !edits.is_null()) else {
        return Ok(vec![Replacement {
            old_string: require_str(args, "old_string")?,
            new_string: require_str(args, "new_string")?,
            replace_all: args.get("replace_all").and_then(Value::as_bool).unwrap_or(false),
        }]);
    };
    if args.get("old_string").is_some() || args.get("new_string").is_some() {
        return Err(invalid(
            "pass either old_string and new_string, or edits, not both".to_string(),
        ));
    }
    let Some(edits) = edits.as_array().filter(|edits| !edits.is_empty()) else {
        return Err(invalid("edits must be a non-empty array".to_string()));
    };
    edits
        .iter()
        .enumerate()
        .map(|(index, edit)| {
            let field = |key: &str| {
                edit.get(key)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| invalid(format!("edits[{index}] needs a string '{key}'")))
            };
            Ok(Replacement {
                old_string: field("old_string")?,
                new_string: field("new_string")?,
                replace_all: edit.get("replace_all").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

/// Applies `replacements` in order to `original`; the error names the replacement that
/// failed when there are several.
pub(crate) fn apply_replacements(
    original: &str,
    replacements: &[(&str, &str, bool)],
    display: &str,
) -> std::result::Result<(String, usize), DevToolError> {
    let mut updated = original.to_string();
    let mut total = 0;
    let several = replacements.len() > 1;
    for (index, (old_string, new_string, replace_all)) in replacements.iter().enumerate() {
        let label =
            if several { format!("{display} (edits[{index}])") } else { display.to_string() };
        if old_string.is_empty() {
            return Err(DevToolError::Other(format!(
                "the target string of {label} is empty; give the exact text to replace"
            )));
        }
        let count = updated.matches(old_string).count();
        if count == 0 {
            return Err(DevToolError::NoMatch(label));
        }
        if count > 1 && !replace_all {
            return Err(DevToolError::Ambiguous { path: label, count });
        }
        updated = if *replace_all {
            updated.replace(old_string, new_string)
        } else {
            updated.replacen(old_string, new_string, 1)
        };
        total += if *replace_all { count } else { 1 };
    }
    Ok((updated, total))
}

/// The content `edit_file` with `args` would leave in a file holding `original`: the
/// replacements of `args` applied in order, or the error the call would fail with
/// (spec 3.1 §7.26: a reviewer sees the edit before it runs).
///
/// # Errors
///
/// Returns the argument, no-match or ambiguity error the call would return.
///
/// # Example
///
/// ```rust
/// use serde_json::json;
///
/// let args = json!({ "path": "a.rs", "edits": [
///     { "old_string": "one", "new_string": "1" },
///     { "old_string": "two", "new_string": "2" }
/// ] });
/// assert_eq!(adk_devtools::tools::edit::preview("one two", &args, "a.rs").unwrap(), "1 2");
/// ```
pub fn preview(original: &str, args: &Value, display: &str) -> Result<String> {
    let replacements = replacements(args)?;
    let pairs: Vec<(&str, &str, bool)> = replacements
        .iter()
        .map(|r| (r.old_string.as_str(), r.new_string.as_str(), r.replace_all))
        .collect();
    Ok(apply_replacements(original, &pairs, display)?.0)
}

#[async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Replace exact strings in a file. The file must have been read with read_file first \
         and be unchanged since; if it changed, read it again. Give old_string and \
         new_string for one replacement, or edits for several in one file, applied in order \
         and all or none. Each target string must occur exactly once unless replace_all is \
         true."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the workspace root." },
                "old_string": { "type": "string", "description": "The exact text to replace (with new_string; omit when using edits)." },
                "new_string": { "type": "string", "description": "The replacement text." },
                "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)." },
                "edits": {
                    "type": "array",
                    "description": "Several replacements in this file, applied in order; all apply or none do.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string" },
                            "new_string": { "type": "string" },
                            "replace_all": { "type": "boolean" }
                        },
                        "required": ["old_string", "new_string"]
                    }
                },
                "expect_sha256": { "type": "string", "description": "Optional: the sha256 the file must have; the edit is refused if it differs." }
            },
            "required": ["path"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        if !self.workspace.is_writable() {
            return Err(DevToolError::ReadOnly("edit_file").into());
        }
        let path = require_str(&args, "path")?;
        let replacements = replacements(&args)?;
        let expect = match args.get("expect_sha256").and_then(Value::as_str) {
            Some(expect) => Some(Precondition::from_expect(expect)?),
            None => None,
        };

        let (resolved, target) = self.workspace.resolve_target(&path)?;
        let display = self.workspace.display(&resolved);
        // The precondition: what the decision bound, else what this agent last saw.
        let expected = match expect {
            Some(Precondition::Sha256(digest)) => digest,
            Some(Precondition::Absent | Precondition::Any) => {
                return Err(DevToolError::Other(format!(
                    "edit_file changes an existing file; '{display}' cannot be expected absent"
                ))
                .into());
            }
            None => match self.workspace.read_digest(ctx.as_ref(), &resolved).await? {
                Some(digest) => digest,
                None => return Err(DevToolError::NotRead(display).into()),
            },
        };

        let original = self.workspace.read_contained(&target).await?;
        let found = sha256_hex(original.as_bytes());
        if found != expected {
            return Err(DevToolError::Stale {
                path: display,
                detail: crate::workspace::StaleFile::Changed { expected, found }.to_string(),
            }
            .into());
        }
        let pairs: Vec<(&str, &str, bool)> = replacements
            .iter()
            .map(|r| (r.old_string.as_str(), r.new_string.as_str(), r.replace_all))
            .collect();
        let (updated, count) = apply_replacements(&original, &pairs, &display)?;
        self.workspace
            .write_contained(&target, updated.as_bytes(), Precondition::Sha256(found))
            .await?;
        self.workspace
            .record_digest(ctx.as_ref(), &resolved, &sha256_hex(updated.as_bytes()))
            .await;

        Ok(json!({
            "path": display,
            "replacements": count,
            "message": format!("Replaced {count} occurrence(s) in {display}"),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacements_apply_in_order_and_name_the_one_that_failed() {
        let (out, count) =
            apply_replacements("a b c", &[("a", "x", false), ("x b", "y", false)], "f").unwrap();
        assert_eq!((out.as_str(), count), ("y c", 2));
        let error =
            apply_replacements("a b", &[("a", "x", false), ("zzz", "y", false)], "f").unwrap_err();
        assert!(matches!(&error, DevToolError::NoMatch(label) if label == "f (edits[1])"));
        let error = apply_replacements("a a", &[("a", "b", false)], "f").unwrap_err();
        assert!(matches!(error, DevToolError::Ambiguous { count: 2, .. }));
        let (out, count) = apply_replacements("a a", &[("a", "b", true)], "f").unwrap();
        assert_eq!((out.as_str(), count), ("b b", 2));
    }

    #[test]
    fn edits_and_a_single_pair_are_exclusive() {
        let both = json!({ "path": "f", "old_string": "a", "new_string": "b",
            "edits": [{ "old_string": "a", "new_string": "b" }] });
        assert!(replacements(&both).is_err());
        assert!(replacements(&json!({ "path": "f", "edits": [] })).is_err());
        let several = json!({ "path": "f", "edits": [
            { "old_string": "a", "new_string": "b" },
            { "old_string": "c", "new_string": "d", "replace_all": true }
        ] });
        let parsed = replacements(&several).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(parsed[1].replace_all);
    }
}
