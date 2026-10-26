//! The harness's edit tools: the paths an Edit-class call writes, as the policy, standing
//! rules and fingerprint approvals read them, and the multi-file tool each model family
//! prefers (spec 3.1 §7.26, §13 Q12).

use serde_json::Value;

/// The multi-file edit tool a model family works best with, by model id prefix, first
/// match wins (spec 3.1 §13 Q12). Every model is offered `write_file`, `edit_file` and
/// `apply_patch`, so the tool surface is one per process (I56); this table only names the
/// one the capability block recommends, as data the eval suite can revise.
///
/// # Example
///
/// ```rust
/// assert!(adk_harness::PREFERRED_MULTI_FILE_EDIT.contains(&("gpt-", "apply_patch")));
/// ```
pub const PREFERRED_MULTI_FILE_EDIT: &[(&str, &str)] = &[
    ("gpt-", "apply_patch"),
    ("o1", "apply_patch"),
    ("o3", "apply_patch"),
    ("o4", "apply_patch"),
    ("codex", "apply_patch"),
    ("claude-", "edit_file"),
    ("gemini-", "edit_file"),
];

/// The multi-file edit tool the capability block recommends for `model`; `edit_file`
/// when no prefix of [`PREFERRED_MULTI_FILE_EDIT`] matches.
pub(crate) fn preferred_multi_file_edit(model: &str) -> &'static str {
    let model = model.rsplit('/').next().unwrap_or(model).to_ascii_lowercase();
    PREFERRED_MULTI_FILE_EDIT
        .iter()
        .find(|(prefix, _)| model.starts_with(prefix))
        .map_or("edit_file", |(_, tool)| tool)
}

/// The tool whose paths are the headers of its patch.
pub(crate) const APPLY_PATCH: &str = "apply_patch";

/// Every path a call writes: `/path` for `write_file`, `edit_file` and other tools, and
/// every header path of an `apply_patch` patch — a move's source and destination
/// included — parsed by the parser the tool itself runs. `None` when the call names no
/// path or its patch does not parse; such a patch writes nothing.
pub(crate) fn edit_paths(tool: &str, args: &Value) -> Option<Vec<String>> {
    if tool == APPLY_PATCH {
        let patch = adk_devtools::patch::parse_patch(args.get("patch")?.as_str()?).ok()?;
        return Some(patch.paths().into_iter().map(str::to_string).collect());
    }
    args.get("path").and_then(Value::as_str).map(|path| vec![path.to_string()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_patch_names_every_header_path_and_other_tools_their_path() {
        let patch = "*** Begin Patch\n*** Add File: a.md\n+x\n*** Update File: src/b.rs\n\
                     *** Move to: src/c.rs\n@@\n-x\n+y\n*** Delete File: d.txt\n*** End Patch";
        assert_eq!(
            edit_paths(APPLY_PATCH, &json!({ "patch": patch })),
            Some(vec!["a.md".into(), "src/b.rs".into(), "src/c.rs".into(), "d.txt".into()])
        );
        assert_eq!(edit_paths(APPLY_PATCH, &json!({ "patch": "nonsense" })), None);
        assert_eq!(edit_paths("edit_file", &json!({ "path": "x.rs" })), Some(vec!["x.rs".into()]));
        assert_eq!(edit_paths("edit_file", &json!({})), None);
    }

    #[test]
    fn each_model_family_has_a_preferred_multi_file_tool() {
        assert_eq!(preferred_multi_file_edit("gpt-5.6-terra"), "apply_patch");
        assert_eq!(preferred_multi_file_edit("openai/o4-mini"), "apply_patch");
        assert_eq!(preferred_multi_file_edit("claude-sonnet-5-5"), "edit_file");
        assert_eq!(preferred_multi_file_edit("gemini-3.7-flash"), "edit_file");
        assert_eq!(preferred_multi_file_edit("scripted"), "edit_file");
    }
}
