//! `read_file` — return the (optionally sliced) contents of a file, or an image or PDF as
//! inline data (spec 3.1 §10 D6).

use std::sync::Arc;

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::error::DevToolError;
use crate::workspace::Workspace;

/// The largest image or PDF `read_file` returns as inline data by default: 4 MiB
/// (spec 3.1 §10 D6). A larger file gets the text stub only.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_devtools::DEFAULT_MAX_INLINE_BYTES, 4 * 1024 * 1024);
/// assert!(adk_devtools::DEFAULT_MAX_INLINE_BYTES < adk_core::MAX_INLINE_DATA_SIZE);
/// ```
pub const DEFAULT_MAX_INLINE_BYTES: usize = 4 * 1024 * 1024;

/// Reads a file from the workspace: UTF-8 text with line numbers, or a PNG, JPEG, GIF, WebP
/// or PDF as an inline data part the model can look at (spec 3.1 §10 D6).
///
/// A file matching the workspace's exclusion patterns ([`Workspace::exclude_globs`]),
/// by its requested path or the path a symlink resolves to, is refused with
/// [`DevToolError::Excluded`]. On Unix the file is opened without following a symlink on
/// any component of the target the containment check resolved, and a file with more
/// than one hard link is refused (spec 3.1 §10 D4). Binary content follows the same rules.
///
/// The type comes from the file's first bytes, never its extension: a text file named
/// `diagram.png` is read as text, and `notes.txt` holding a PNG is returned as an image.
/// The result of a binary read is `{ "response": { path, mime_type, bytes, content },
/// "inline_data": [{ mime_type, data }] }`, which the agent loop turns into a function
/// response with an inline part; `content` is a stub naming the type and size, so a
/// provider that cannot take the bytes still learns what the file is. Over
/// [`max_inline_bytes`](Self::max_inline_bytes), or under [`text_only`](Self::text_only),
/// only the stub is returned. The bytes are never interpreted here (spec 3.1 §9.2 I55).
///
/// # Example
///
/// ```rust
/// use adk_devtools::{ReadFileTool, Workspace};
///
/// let read = ReadFileTool::new(Workspace::new("./my-repo")).max_inline_bytes(1024 * 1024);
/// # let _ = read;
/// ```
pub struct ReadFileTool {
    workspace: Workspace,
    max_inline_bytes: usize,
    inline_data: bool,
}

impl ReadFileTool {
    /// Create a `read_file` tool bound to `workspace`.
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace, max_inline_bytes: DEFAULT_MAX_INLINE_BYTES, inline_data: true }
    }

    /// The largest image or PDF returned as inline data; capped at
    /// [`adk_core::MAX_INLINE_DATA_SIZE`]. A larger file gets the text stub only.
    #[must_use]
    pub fn max_inline_bytes(mut self, bytes: usize) -> Self {
        self.max_inline_bytes = bytes.min(adk_core::MAX_INLINE_DATA_SIZE);
        self
    }

    /// Returns the text stub for images and PDFs and never their bytes, for a model
    /// without vision.
    #[must_use]
    pub fn text_only(mut self) -> Self {
        self.inline_data = false;
        self
    }
}

/// The MIME type of `bytes` when they start like a PNG, JPEG, GIF, WebP or PDF
/// (spec 3.1 §10 D6: magic bytes, not the extension).
///
/// # Example
///
/// ```rust
/// use adk_devtools::sniff_inline_mime;
///
/// assert_eq!(sniff_inline_mime(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
/// assert_eq!(sniff_inline_mime(b"%PDF-1.7\n"), Some("application/pdf"));
/// assert_eq!(sniff_inline_mime(b"fn main() {}"), None);
/// ```
pub fn sniff_inline_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xFF\xD8\xFF") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else {
        None
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    /// Reads a workspace file on disk.
    fn egress(&self, _args: &serde_json::Value) -> adk_core::egress::ToolEgress {
        adk_core::egress::ToolEgress::None
    }

    fn description(&self) -> &str {
        "Read a file from the workspace. Text comes back with line numbers, with optional \
         `offset` (1-based start line) and `limit` (number of lines). A PNG, JPEG, GIF, WebP \
         or PDF comes back as an attached image or document you can look at, with its type \
         and size."
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
                "path": { "type": "string", "description": "Path relative to the workspace root." },
                "offset": { "type": "integer", "description": "1-based first line to return (optional, text only)." },
                "limit": { "type": "integer", "description": "Maximum number of lines to return (optional, text only)." }
            },
            "required": ["path"]
        }))
    }

    async fn execute(&self, ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let path = require_str(&args, "path")?;
        let (resolved, target) = self.workspace.resolve_target(&path)?;
        // A link inside the workspace can lead to an excluded file under another name.
        if self.workspace.is_excluded(&resolved) || self.workspace.is_excluded(&target) {
            return Err(DevToolError::Excluded(self.workspace.display(&resolved)).into());
        }

        let bytes = self.workspace.read_contained_bytes(&target).await?;
        self.workspace
            .record_digest(ctx.as_ref(), &resolved, &crate::read_ledger::sha256_hex(&bytes))
            .await;
        let display = self.workspace.display(&resolved);

        if let Some(mime_type) = sniff_inline_mime(&bytes) {
            let size = bytes.len();
            let attached = self.inline_data && size <= self.max_inline_bytes;
            let kind = if mime_type == "application/pdf" { "PDF document" } else { "image" };
            let content = if attached {
                format!("[{kind} {mime_type}, {size} bytes, attached as inline data]")
            } else if self.inline_data {
                format!(
                    "[{kind} {mime_type}, {size} bytes; over the {} byte inline cap, so the \
                     bytes are not attached]",
                    self.max_inline_bytes
                )
            } else {
                format!("[{kind} {mime_type}, {size} bytes; this model receives no binary data]")
            };
            let response = json!({
                "path": display,
                "mime_type": mime_type,
                "bytes": size,
                "content": content,
                "inline": attached,
            });
            return Ok(if attached {
                json!({
                    "response": response,
                    "inline_data": [{ "mime_type": mime_type, "data": bytes }],
                })
            } else {
                response
            });
        }

        let contents = String::from_utf8(bytes).map_err(|_| {
            DevToolError::Other(format!(
                "'{display}' is neither UTF-8 text nor a PNG, JPEG, GIF, WebP or PDF; read_file \
                 returns text and those image and document types only"
            ))
        })?;

        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
        let limit = args.get("limit").and_then(Value::as_u64).map(|v| v as usize);

        let mut lines = String::new();
        let mut total = 0usize;
        let mut shown = 0usize;
        for (idx, line) in contents.lines().enumerate() {
            total += 1;
            let line_no = idx + 1;
            if line_no < offset {
                continue;
            }
            if let Some(limit) = limit
                && shown >= limit
            {
                break;
            }
            lines.push_str(&format!("{line_no:>6}\t{line}\n"));
            shown += 1;
        }

        Ok(json!({
            "path": display,
            "content": lines,
            "total_lines": total,
            "returned_lines": shown,
        }))
    }
}

pub(crate) fn require_str(args: &Value, key: &str) -> Result<String> {
    args.get(key).and_then(Value::as_str).map(str::to_string).ok_or_else(|| {
        adk_core::AdkError::tool(format!("missing required string argument '{key}'"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing_reads_magic_bytes_only() {
        assert_eq!(sniff_inline_mime(b"\xFF\xD8\xFF\xE0JFIF"), Some("image/jpeg"));
        assert_eq!(sniff_inline_mime(b"GIF89a\x01\x00"), Some("image/gif"));
        assert_eq!(sniff_inline_mime(b"RIFF\x00\x00\x00\x00WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_inline_mime(b"RIFF\x00\x00\x00\x00WAVEfmt "), None);
        assert_eq!(sniff_inline_mime(b"%PDF-1.4"), Some("application/pdf"));
        assert_eq!(sniff_inline_mime(b"PNG but not really"), None);
        assert_eq!(sniff_inline_mime(b""), None);
    }
}
