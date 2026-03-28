use serde::{Deserialize, Serialize};

/// A generic paginated list response.
///
/// Used by Models API, Files API, Skills API, and Batches API for paginated results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaginatedList<T> {
    /// The list of items in this page.
    pub data: Vec<T>,
    /// Whether there are more results available.
    pub has_more: bool,
    /// The ID of the first item in this page (for backward pagination).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_id: Option<String>,
    /// The ID of the last item in this page (for forward pagination).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_id: Option<String>,
}

impl<T> PaginatedList<T> {
    /// Create a new `PaginatedList`.
    pub fn new(
        data: Vec<T>,
        has_more: bool,
        first_id: Option<String>,
        last_id: Option<String>,
    ) -> Self {
        Self { data, has_more, first_id, last_id }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serialization() {
        let list = PaginatedList::new(
            vec!["item1".to_string(), "item2".to_string()],
            true,
            Some("first".to_string()),
            Some("last".to_string()),
        );
        let json = serde_json::to_value(&list).unwrap();
        assert_eq!(
            json,
            json!({
                "data": ["item1", "item2"],
                "has_more": true,
                "first_id": "first",
                "last_id": "last"
            })
        );
    }

    #[test]
    fn deserialization() {
        let json = json!({
            "data": [1, 2, 3],
            "has_more": false
        });
        let list: PaginatedList<i32> = serde_json::from_value(json).unwrap();
        assert_eq!(list.data, vec![1, 2, 3]);
        assert!(!list.has_more);
        assert!(list.first_id.is_none());
        assert!(list.last_id.is_none());
    }
}
