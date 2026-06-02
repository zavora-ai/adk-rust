use serde::{Deserialize, Serialize};

/// Declarative workspace definition specifying initial contents.
///
/// A `Manifest` describes the files, directories, and git repositories
/// that should be created in a sandbox workspace during provisioning.
/// Entries are processed in order.
///
/// # Example
///
/// ```rust,ignore
/// use adk_sandbox::workspace::Manifest;
/// use adk_sandbox::workspace::ManifestEntry;
///
/// let manifest = Manifest {
///     entries: vec![
///         ManifestEntry::Directory { path: "src".to_string() },
///         ManifestEntry::File {
///             path: "src/main.rs".to_string(),
///             content: b"fn main() {}".to_vec(),
///         },
///     ],
/// };
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Ordered list of workspace entries to create during provisioning.
    pub entries: Vec<ManifestEntry>,
}

impl Manifest {
    /// Creates a new manifest with the given entries.
    pub fn new(entries: Vec<ManifestEntry>) -> Self {
        Self { entries }
    }
}

/// A single entry in a workspace manifest.
///
/// Each variant represents a different kind of workspace content that
/// can be provisioned. All target paths are relative to the workspace root.
///
/// # Serialization
///
/// Uses internally tagged representation with `"type"` as the tag field
/// and snake_case variant names:
///
/// ```json
/// { "type": "file", "path": "src/main.rs", "content": [102, 110] }
/// { "type": "directory", "path": "src" }
/// { "type": "git_repo", "url": "https://github.com/...", "branch": null, "path": "repo" }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ManifestEntry {
    /// An inline file with content.
    File {
        /// Target path relative to workspace root.
        path: String,
        /// File content as UTF-8 or base64-encoded bytes.
        content: Vec<u8>,
    },
    /// An empty directory.
    Directory {
        /// Target path relative to workspace root.
        path: String,
    },
    /// A git repository to clone.
    GitRepo {
        /// Repository URL (HTTPS or SSH).
        url: String,
        /// Optional branch to check out (defaults to repo default).
        branch: Option<String>,
        /// Target path relative to workspace root.
        path: String,
    },
}
