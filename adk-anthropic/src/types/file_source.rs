use serde::{Deserialize, Serialize};

/// A file source referencing a server-side file by ID.
///
/// Used in image and document blocks to reference files uploaded via the Files API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSource {
    /// The ID of the uploaded file.
    pub file_id: String,
}

impl FileSource {
    /// Create a new `FileSource` with the given file ID.
    pub fn new(file_id: impl Into<String>) -> Self {
        Self { file_id: file_id.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialization() {
        let source = FileSource::new("file-abc123");
        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json, json!({"file_id": "file-abc123"}));
    }

    #[test]
    fn deserialization() {
        let json = json!({"file_id": "file-xyz789"});
        let source: FileSource = serde_json::from_value(json).unwrap();
        assert_eq!(source.file_id, "file-xyz789");
    }
}
