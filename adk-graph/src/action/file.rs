//! File action node executor.
//!
//! Supports:
//! - **read**: Read file contents, parse as json/csv/text.
//! - **write**: Write content to file, with optional dir creation and append mode.
//! - **delete**: Remove a file.
//! - **list**: List directory contents with optional recursion and glob filtering.

use adk_action::{ActionError, FileFormat, FileNodeConfig, FileOperation, interpolate_variables};
use serde_json::Value;

use crate::error::{GraphError, Result};
use crate::node::{NodeContext, NodeOutput};

/// Execute a File action node.
pub async fn execute_file(config: &FileNodeConfig, ctx: &NodeContext) -> Result<NodeOutput> {
    let state = &ctx.state;
    let node_id = &config.standard.id;
    let output_key = &config.standard.mapping.output_key;

    // Resolve file path with variable interpolation
    let raw_path = config.local.as_ref().map(|l| l.path.as_str()).unwrap_or("");
    let path = interpolate_variables(raw_path, state);

    match config.operation {
        FileOperation::Read => execute_read(config, &path, node_id, output_key).await,
        FileOperation::Write => execute_write(config, &path, node_id, output_key).await,
        FileOperation::Delete => execute_delete(&path, node_id, output_key).await,
        FileOperation::List => execute_list(config, &path, node_id, output_key).await,
    }
}

async fn execute_read(
    config: &FileNodeConfig,
    path: &str,
    node_id: &str,
    output_key: &str,
) -> Result<NodeOutput> {
    tracing::debug!(node = %node_id, path = %path, "reading file");

    let content =
        tokio::fs::read_to_string(path).await.map_err(|e| GraphError::NodeExecutionFailed {
            node: node_id.to_string(),
            message: ActionError::FileRead(format!("failed to read '{path}': {e}")).to_string(),
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
    path: &str,
    node_id: &str,
    output_key: &str,
) -> Result<NodeOutput> {
    let write_cfg = config.write.as_ref().ok_or_else(|| GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: "write operation missing write configuration".into(),
    })?;

    // Create parent directories if configured
    if write_cfg.create_dirs {
        if let Some(parent) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                GraphError::NodeExecutionFailed {
                    node: node_id.to_string(),
                    message: ActionError::FileWrite(format!(
                        "failed to create directories for '{path}': {e}"
                    ))
                    .to_string(),
                }
            })?;
        }
    }

    let content_str = match &write_cfg.content {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };

    if write_cfg.append {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .map_err(|e| GraphError::NodeExecutionFailed {
                node: node_id.to_string(),
                message: ActionError::FileWrite(format!("failed to open '{path}' for append: {e}"))
                    .to_string(),
            })?;
        file.write_all(content_str.as_bytes()).await.map_err(|e| {
            GraphError::NodeExecutionFailed {
                node: node_id.to_string(),
                message: ActionError::FileWrite(format!("failed to append to '{path}': {e}"))
                    .to_string(),
            }
        })?;
    } else {
        tokio::fs::write(path, &content_str).await.map_err(|e| {
            GraphError::NodeExecutionFailed {
                node: node_id.to_string(),
                message: ActionError::FileWrite(format!("failed to write '{path}': {e}"))
                    .to_string(),
            }
        })?;
    }

    tracing::debug!(node = %node_id, path = %path, append = write_cfg.append, "wrote file");

    Ok(NodeOutput::new()
        .with_update(output_key, serde_json::json!({ "path": path, "written": true })))
}

async fn execute_delete(path: &str, node_id: &str, output_key: &str) -> Result<NodeOutput> {
    tracing::debug!(node = %node_id, path = %path, "deleting file");

    tokio::fs::remove_file(path).await.map_err(|e| GraphError::NodeExecutionFailed {
        node: node_id.to_string(),
        message: ActionError::FileDelete(format!("failed to delete '{path}': {e}")).to_string(),
    })?;

    Ok(NodeOutput::new()
        .with_update(output_key, serde_json::json!({ "path": path, "deleted": true })))
}

async fn execute_list(
    config: &FileNodeConfig,
    path: &str,
    node_id: &str,
    output_key: &str,
) -> Result<NodeOutput> {
    let list_cfg = config.list.as_ref();
    let recursive = list_cfg.is_some_and(|l| l.recursive);
    let pattern = list_cfg.and_then(|l| l.pattern.as_deref());

    tracing::debug!(
        node = %node_id,
        path = %path,
        recursive = recursive,
        pattern = ?pattern,
        "listing directory"
    );

    let entries = list_directory(path, recursive, pattern).await.map_err(|e| {
        GraphError::NodeExecutionFailed {
            node: node_id.to_string(),
            message: ActionError::FileRead(format!("failed to list '{path}': {e}")).to_string(),
        }
    })?;

    let entries_json: Vec<Value> = entries.into_iter().map(Value::String).collect();

    Ok(NodeOutput::new().with_update(output_key, Value::Array(entries_json)))
}

async fn list_directory(
    path: &str,
    recursive: bool,
    pattern: Option<&str>,
) -> std::io::Result<Vec<String>> {
    let mut entries = Vec::new();
    let mut dirs_to_visit = vec![std::path::PathBuf::from(path)];

    while let Some(dir) = dirs_to_visit.pop() {
        let mut read_dir = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let entry_path = entry.path();
            let path_str = entry_path.to_string_lossy().to_string();

            if entry_path.is_dir() && recursive {
                dirs_to_visit.push(entry_path);
            } else if entry_path.is_file() {
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
