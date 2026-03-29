use std::ops::Add;

use serde::{Deserialize, Serialize};

/// Information about server tool usage for a message.
#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServerToolUsage {
    /// The number of web search tool requests.
    pub web_search_requests: i32,
}

impl ServerToolUsage {
    /// Create a new `ServerToolUsage` with the given web search requests count.
    pub fn new(web_search_requests: i32) -> Self {
        Self { web_search_requests }
    }
}

impl Add for ServerToolUsage {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self { web_search_requests: self.web_search_requests + rhs.web_search_requests }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, to_value};

    #[test]
    fn server_tool_usage_serialization() {
        let usage = ServerToolUsage::new(5);
        let json = to_value(usage).unwrap();

        assert_eq!(
            json,
            json!({
                "web_search_requests": 5
            })
        );
    }

    #[test]
    fn server_tool_usage_deserialization() {
        let json = json!({
            "web_search_requests": 5
        });

        let usage: ServerToolUsage = serde_json::from_value(json).unwrap();
        assert_eq!(usage.web_search_requests, 5);
    }

    #[test]
    fn add_server_tool_usage() {
        let usage1 = ServerToolUsage::new(5);
        let usage2 = ServerToolUsage::new(3);
        let result = usage1 + usage2;

        assert_eq!(result.web_search_requests, 8);
    }
}
