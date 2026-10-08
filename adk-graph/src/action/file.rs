//! File action node executor.
//!
//! Supports:
//! - **read**: Read file contents, parse as json/csv/text.
//! - **write**: Write content to file, with optional dir creation and append mode.
//! - **delete**: Remove a file.
//! - **list**: List directory contents with optional recursion and glob filtering.
//!
//! # Confinement
//!
//! The path is interpolated from workflow state, so it is treated as untrusted.
//! Every operation is confined to a set of allowed roots — the current working
//! directory unless [`ActionNodeExecutor::with_file_roots`] names others:
//!
//! | Path | Outcome |
//! |------|---------|
//! | Relative | Resolved against the first root. |
//! | Contains a `..` component | Rejected. |
//! | Resolves outside every root, including through a symbolic link | Rejected. |
//! | Ends in a dangling symbolic link | Rejected. |
//!
//! Listing does not follow or report symbolic links.
//!
//! [`ActionNodeExecutor::with_file_roots`]: super::ActionNodeExecutor::with_file_roots

use std::path::{Component, Path, PathBuf};

use adk_action::{ActionError, FileFormat, FileNodeConfig, FileOperation, interpolate_variables};
use serde_json::Value;

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute a File action node confined to the current working directory.
///
/// Equivalent to [`execute_file_in`] with no roots.
///
/// # Errors
///
/// See [`execute_file_in`].
pub async fn execute_file(config: &FileNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    execute_file_in(config, ctx, &[]).await
}

/// Execute a File action node confined to `roots`.
///
/// An empty `roots` confines the node to the current working directory. See
/// the [module documentation](self) for how a path is resolved.
///
/// # Example
///
/// ```rust,ignore
/// use std::path::PathBuf;
/// use adk_graph::action::file::execute_file_in;
///
/// let output = execute_file_in(&config, &ctx, &[PathBuf::from("/srv/workflow-data")]).await?;
/// ```
///
/// # Errors
///
/// Returns [`GraphError::NodeExecutionFailed`] when the path is empty, contains
/// `..`, resolves outside every root, or ends in a dangling symbolic link; when
/// a root cannot be resolved; or when the file operation itself fails.
pub async fn execute_file_in(
    config: &FileNodeConfig,
    ctx: &NodeContext,
    roots: &[PathBuf],
) -> Result<NodeOutput> {
    let state = &ctx.state;
    let node_id = &config.standard.id;
    let output_key = &config.standard.mapping.output_key;

    // Resolve file path with variable interpolation
    let raw_path = config.local.as_ref().map(|l| l.path.as_str()).unwrap_or("");
    let interpolated = interpolate_variables(raw_path, state);
    let path = confine(&interpolated, roots).await.map_err(|message| {
        GraphError::NodeExecutionFailed { node: node_id.to_string(), message }
    })?;

    match config.operation {
        FileOperation::Read => execute_read(config, &path, node_id, output_key).await,
        FileOperation::Write => execute_write(config, &path, node_id, output_key).await,
        FileOperation::Delete => execute_delete(&path, node_id, output_key).await,
        FileOperation::List => execute_list(config, &path, node_id, output_key).await,
    }
}

/// Resolves `raw` inside one of `roots`, or explains why it cannot be.
async fn confine(raw: &str, roots: &[PathBuf]) -> std::result::Result<PathBuf, String> {
    if raw.is_empty() {
        return Err("file node has no path; set `local.path`".to_string());
    }
    let requested = Path::new(raw);
    if requested.components().any(|component| matches!(component, Component::ParentDir)) {
        return Err(format!(
            "file path '{raw}' contains '..'; name a path inside the allowed roots instead"
        ));
    }

    let configured = if roots.is_empty() {
        vec![std::env::current_dir().map_err(|e| {
            format!("the current working directory, the default file root, cannot be read: {e}")
        })?]
    } else {
        roots.to_vec()
    };
    let mut allowed = Vec::with_capacity(configured.len());
    for root in &configured {
        let canonical = tokio::fs::canonicalize(root).await.map_err(|e| {
            format!("allowed file root '{}' cannot be resolved: {e}", root.display())
        })?;
        allowed.push(canonical);
    }

    let joined =
        if requested.is_absolute() { requested.to_path_buf() } else { allowed[0].join(requested) };
    let resolved = resolve_symlinks(&joined)
        .await
        .map_err(|e| format!("file path '{raw}' cannot be resolved: {e}"))?;
    if allowed.iter().any(|root| resolved.starts_with(root)) {
        Ok(resolved)
    } else {
        Err(format!(
            "file path '{raw}' resolves to '{}', outside the allowed roots; configure them with \
             `ActionNodeExecutor::with_file_roots`",
            resolved.display()
        ))
    }
}

/// Canonicalizes the longest existing prefix of `path` and appends the rest.
///
/// The rest cannot hold `..` (rejected earlier), so the result names the file
/// an operation would touch. A dangling link would let a write land wherever
/// the link points, so one is an error.
async fn resolve_symlinks(path: &Path) -> std::io::Result<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match tokio::fs::canonicalize(&existing).await {
            Ok(mut resolved) => {
                resolved.extend(missing.iter().rev());
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if tokio::fs::symlink_metadata(&existing).await.is_ok() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("'{}' is a dangling symbolic link", existing.display()),
                    ));
                }
                let Some(name) = existing.file_name().map(std::ffi::OsStr::to_os_string) else {
                    return Err(error);
                };
                missing.push(name);
                if !existing.pop() {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

async fn execute_read(
    config: &FileNodeConfig,
    path: &Path,
    node_id: &str,
    output_key: &str,
) -> Result<NodeOutput> {
    let path_display = path.display();
    tracing::debug!(node = %node_id, path = %path_display, "reading file");

    let content =
        tokio::fs::read_to_string(path).await.map_err(|e| GraphError::NodeExecutionFailed {
            node: node_id.to_string(),
            message: ActionError::FileRead(format!("failed to read '{path_display}': {e}"))
                .to_string(),
        })?;

    let format = config.parse.as_ref().map(|p| &p.format).unwrap_or(&FileFormat::Text);

    let parsed = match format {
        FileFormat::Json => serde_json::from_str::<Value>(&content).map_err(|e| {
            GraphError::NodeExecutionFailed {
                node: node_id.to_string(),
                message: ActionError::FileParse(format!("JSON parse failed: {e}")).to_string(),
            }
        })?,
        FileFormat::Csv => parse_csv(
            &content,
            config.parse.as_ref().and_then(|p| p.csv_options.as_ref()),
            node_id,
        )?,
        FileFormat::Text | FileFormat::Binary => Value::String(content),
        FileFormat::Xml => {
            // XML parsing is a stretch goal; return as text
            Value::String(content)
        }
    };

    Ok(NodeOutput::new().with_update(output_key, parsed))
}

fn parse_csv(
    content: &str,
    csv_options: Option<&adk_action::CsvOptions>,
    _node_id: &str,
) -> Result<Value> {
    let delimiter = csv_options.map(|o| o.delimiter.as_str()).unwrap_or(",");
    let has_header = csv_options.map(|o| o.has_header).unwrap_or(true);

    // Simple line-based CSV parsing (no external csv crate dependency)
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Ok(Value::Array(vec![]));
    }

    let delimiter_char = delimiter.chars().next().unwrap_or(',');

    if has_header && lines.len() > 1 {
        let headers: Vec<&str> = lines[0].split(delimiter_char).map(str::trim).collect();
        let rows: Vec<Value> = lines[1..]
            .iter()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let fields: Vec<&str> = line.split(delimiter_char).map(str::trim).collect();
                let mut map = serde_json::Map::new();
                for (i, header) in headers.iter().enumerate() {
                    let val = fields.get(i).unwrap_or(&"");
                    map.insert(header.to_string(), Value::String(val.to_string()));
                }
                Value::Object(map)
            })
            .collect();
        Ok(Value::Array(rows))
    } else {
        let rows: Vec<Value> = lines
            .iter()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let fields: Vec<Value> = line
                    .split(delimiter_char)
                    .map(|f| Value::String(f.trim().to_string()))
                    .collect();
                Value::Array(fields)
            })
            .collect();
        Ok(Value::Array(rows))
    }
}

async fn execute_write(
    config: &FileNodeConfig,
    path: &Path,
    node_id: &str,
    output_key: &str,
) -> Result<NodeOutput> {
    let write_cfg = config.write.as_ref().ok_or_else(|| GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: "write operation missing write configuration".into(),
    })?;
    let path_display = path.display().to_string();
    let path_display = path_display.as_str();

    // Create parent directories if configured
    if write_cfg.create_dirs
        && let Some(parent) = path.parent()
    {
        tokio::fs::create_dir_all(parent).await.map_err(|e| GraphError::NodeExecutionFailed {
            node: node_id.to_string(),
            message: ActionError::FileWrite(format!(
                "failed to create directories for '{path_display}': {e}"
            ))
            .to_string(),
        })?;
    }

    let content_str = match &write_cfg.content {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };

    if write_cfg.append {
        use tokio::io::AsyncWriteExt;
        let mut file =
            tokio::fs::OpenOptions::new().create(true).append(true).open(path).await.map_err(
                |e| GraphError::NodeExecutionFailed {
                    node: node_id.to_string(),
                    message: ActionError::FileWrite(format!(
                        "failed to open '{path_display}' for append: {e}"
                    ))
                    .to_string(),
                },
            )?;
        file.write_all(content_str.as_bytes()).await.map_err(|e| {
            GraphError::NodeExecutionFailed {
                node: node_id.to_string(),
                message: ActionError::FileWrite(format!(
                    "failed to append to '{path_display}': {e}"
                ))
                .to_string(),
            }
        })?;
    } else {
        tokio::fs::write(path, &content_str).await.map_err(|e| {
            GraphError::NodeExecutionFailed {
                node: node_id.to_string(),
                message: ActionError::FileWrite(format!("failed to write '{path_display}': {e}"))
                    .to_string(),
            }
        })?;
    }

    tracing::debug!(node = %node_id, path = %path_display, append = write_cfg.append, "wrote file");

    Ok(NodeOutput::new()
        .with_update(output_key, serde_json::json!({ "path": path_display, "written": true })))
}

async fn execute_delete(path: &Path, node_id: &str, output_key: &str) -> Result<NodeOutput> {
    let path_display = path.display().to_string();
    tracing::debug!(node = %node_id, path = %path_display, "deleting file");

    tokio::fs::remove_file(path).await.map_err(|e| GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: ActionError::FileDelete(format!("failed to delete '{path_display}': {e}"))
            .to_string(),
    })?;

    Ok(NodeOutput::new()
        .with_update(output_key, serde_json::json!({ "path": path_display, "deleted": true })))
}

async fn execute_list(
    config: &FileNodeConfig,
    path: &Path,
    node_id: &str,
    output_key: &str,
) -> Result<NodeOutput> {
    let list_cfg = config.list.as_ref();
    let recursive = list_cfg.is_some_and(|l| l.recursive);
    let pattern = list_cfg.and_then(|l| l.pattern.as_deref());
    let path_display = path.display();

    tracing::debug!(
        node = %node_id,
        path = %path_display,
        recursive = recursive,
        pattern = ?pattern,
        "listing directory"
    );

    let entries = list_directory(path, recursive, pattern).await.map_err(|e| {
        GraphError::NodeExecutionFailed {
            node: node_id.to_string(),
            message: ActionError::FileRead(format!("failed to list '{path_display}': {e}"))
                .to_string(),
        }
    })?;

    let entries_json: Vec<Value> = entries.into_iter().map(Value::String).collect();

    Ok(NodeOutput::new().with_update(output_key, Value::Array(entries_json)))
}

async fn list_directory(
    path: &Path,
    recursive: bool,
    pattern: Option<&str>,
) -> std::io::Result<Vec<String>> {
    let mut entries = Vec::new();
    let mut dirs_to_visit = vec![path.to_path_buf()];

    while let Some(dir) = dirs_to_visit.pop() {
        let mut read_dir = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let entry_path = entry.path();
            let path_str = entry_path.to_string_lossy().to_string();
            // Not followed: a link inside a root may point anywhere.
            let file_type = entry.file_type().await?;

            if file_type.is_dir() && recursive {
                dirs_to_visit.push(entry_path);
            } else if file_type.is_file() {
                // Apply glob pattern filter if configured
                if let Some(pat) = pattern {
                    if matches_glob(
                        entry_path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
                        pat,
                    ) {
                        entries.push(path_str);
                    }
                } else {
                    entries.push(path_str);
                }
            }
        }
    }

    entries.sort();
    Ok(entries)
}

/// Simple glob matching supporting `*` and `?` wildcards.
fn matches_glob(name: &str, pattern: &str) -> bool {
    let mut name_chars = name.chars().peekable();
    let mut pat_chars = pattern.chars().peekable();

    while let Some(&pc) = pat_chars.peek() {
        match pc {
            '*' => {
                pat_chars.next();
                if pat_chars.peek().is_none() {
                    return true;
                }
                while name_chars.peek().is_some() {
                    let remaining_name: String = name_chars.clone().collect();
                    let remaining_pat: String = pat_chars.clone().collect();
                    if matches_glob(&remaining_name, &remaining_pat) {
                        return true;
                    }
                    name_chars.next();
                }
                return false;
            }
            '?' => {
                pat_chars.next();
                if name_chars.next().is_none() {
                    return false;
                }
            }
            c => {
                pat_chars.next();
                if name_chars.next() != Some(c) {
                    return false;
                }
            }
        }
    }

    name_chars.peek().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the system temp dir, removed on drop.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("adk-graph-file-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("create temp root");
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn a_relative_path_resolves_inside_the_root() {
        let root = TempRoot::new();
        let resolved = confine("reports/out.json", std::slice::from_ref(&root.0)).await.unwrap();
        let canonical_root = std::fs::canonicalize(&root.0).unwrap();
        assert_eq!(resolved, canonical_root.join("reports").join("out.json"));
    }

    #[tokio::test]
    async fn a_parent_component_is_rejected() {
        let root = TempRoot::new();
        let error = confine("../escape.txt", std::slice::from_ref(&root.0)).await.unwrap_err();
        assert!(error.contains("contains '..'"), "{error}");
    }

    #[tokio::test]
    async fn an_absolute_path_outside_the_root_is_rejected() {
        let root = TempRoot::new();
        let outside = TempRoot::new();
        let target = outside.0.join("secret.txt");
        let error =
            confine(&target.to_string_lossy(), std::slice::from_ref(&root.0)).await.unwrap_err();
        assert!(error.contains("outside the allowed roots"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_symbolic_link_out_of_the_root_is_rejected() {
        let root = TempRoot::new();
        let outside = TempRoot::new();
        std::os::unix::fs::symlink(&outside.0, root.0.join("link")).unwrap();

        let error = confine("link/secret.txt", std::slice::from_ref(&root.0)).await.unwrap_err();
        assert!(error.contains("outside the allowed roots"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dangling_symbolic_link_is_rejected() {
        let root = TempRoot::new();
        let outside = TempRoot::new();
        std::os::unix::fs::symlink(outside.0.join("missing.txt"), root.0.join("dangling")).unwrap();

        let error = confine("dangling", std::slice::from_ref(&root.0)).await.unwrap_err();
        assert!(error.contains("dangling symbolic link"), "{error}");
    }
}
