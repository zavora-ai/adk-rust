use serde::{Deserialize, Serialize};

/// A file object returned by the Files API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileObject {
    /// Unique file identifier.
    pub id: String,
    /// Original filename.
    pub filename: String,
    /// Unix timestamp of creation.
    pub created_at: i64,
    /// File size in bytes.
    pub size: u64,
    /// Purpose of the file (e.g. "assistants").
    pub purpose: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip() {
        let obj = FileObject {
            id: "file-abc123".to_string(),
            filename: "document.pdf".to_string(),
            created_at: 1700000000,
            size: 1024,
            purpose: "assistants".to_string(),
        };
        let json = serde_json::to_value(&obj).unwrap();
        assert_eq!(
            json,
            json!({
                "id": "file-abc123",
                "filename": "document.pdf",
                "created_at": 1700000000,
                "size": 1024,
                "purpose": "assistants"
            })
        );
        let deserialized: FileObject = serde_json::from_value(json).unwrap();
        assert_eq!(obj, deserialized);
    }
}
