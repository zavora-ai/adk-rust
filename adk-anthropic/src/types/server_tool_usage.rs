use std::ops::Add;

use serde::{Deserialize, Serialize};

/// Information about server tool usage for a message.
///
/// # Example
///
/// ```
/// use adk_anthropic::ServerToolUsage;
///
/// let usage: ServerToolUsage =
///     serde_json::from_value(serde_json::json!({ "web_fetch_requests": 2 })).unwrap();
/// assert_eq!(usage, ServerToolUsage::new(0).with_web_fetch_requests(2));
/// ```
#[derive(Debug, Copy, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ServerToolUsage {
    /// The number of web search tool requests.
    #[serde(default)]
    pub web_search_requests: i32,

    /// The number of web fetch tool requests.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub web_fetch_requests: i32,

    /// The number of code execution tool requests.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub code_execution_requests: i32,
}

fn is_zero(count: &i32) -> bool {
    *count == 0
}

impl ServerToolUsage {
    /// Create a new `ServerToolUsage` with the given web search requests count.
    pub fn new(web_search_requests: i32) -> Self {
        Self { web_search_requests, ..Self::default() }
    }

    /// Set the number of web fetch tool requests.
    pub fn with_web_fetch_requests(mut self, web_fetch_requests: i32) -> Self {
        self.web_fetch_requests = web_fetch_requests;
        self
    }

    /// Set the number of code execution tool requests.
    pub fn with_code_execution_requests(mut self, code_execution_requests: i32) -> Self {
        self.code_execution_requests = code_execution_requests;
        self
    }
}

impl Add for ServerToolUsage {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self {
            web_search_requests: self.web_search_requests + rhs.web_search_requests,
            web_fetch_requests: self.web_fetch_requests + rhs.web_fetch_requests,
            code_execution_requests: self.code_execution_requests + rhs.code_execution_requests,
        }
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

    #[test]
    fn web_fetch_and_code_execution_counts_round_trip_and_add() {
        let json = json!({ "web_search_requests": 1, "web_fetch_requests": 2, "code_execution_requests": 3 });
        let usage: ServerToolUsage = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            usage,
            ServerToolUsage::new(1).with_web_fetch_requests(2).with_code_execution_requests(3)
        );
        assert_eq!(to_value(usage).unwrap(), json);
        assert_eq!(
            usage + usage,
            ServerToolUsage::new(2).with_web_fetch_requests(4).with_code_execution_requests(6)
        );
    }
}
