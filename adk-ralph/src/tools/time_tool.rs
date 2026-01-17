//! Get Time Tool for returning current date and time.
//!
//! This tool provides the current datetime in multiple formats,
//! useful for general queries and logging.
//!
//! ## Requirements Validated
//!
//! - 2.3: THE Orchestrator_Agent SHALL have access to `get_time` tool

use adk_core::{Result, Tool, ToolContext};
use async_trait::async_trait;
use chrono::{DateTime, Local, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool for getting the current date and time.
///
/// Returns the current datetime in multiple formats:
/// - ISO 8601 (UTC)
/// - Human-readable formatted string
/// - Unix timestamp
///
/// # Input
///
/// ```json
/// {
///     "timezone": "local"
/// }
/// ```
///
/// # Output
///
/// ```json
/// {
///     "datetime": "2026-01-14T10:30:00Z",
///     "formatted": "Wednesday, January 14, 2026 at 10:30 AM",
///     "date": "2026-01-14",
///     "time": "10:30:00",
///     "timestamp": 1768412200,
///     "timezone": "UTC"
/// }
/// ```
pub struct GetTimeTool;

impl GetTimeTool {
    /// Create a new GetTimeTool.
    pub fn new() -> Self {
        Self
    }
}

impl Default for GetTimeTool {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GetTimeTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GetTimeTool").finish()
    }
}

#[async_trait]
impl Tool for GetTimeTool {
    fn name(&self) -> &str {
        "get_time"
    }

    fn description(&self) -> &str {
        "Get the current date and time in multiple formats. Useful for answering questions about the current time or for logging purposes."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "timezone": {
                    "type": "string",
                    "enum": ["utc", "local"],
                    "description": "Timezone for the response: 'utc' for UTC time, 'local' for local system time"
                }
            }
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        #[derive(Deserialize, Default)]
        struct Args {
            #[serde(default)]
            timezone: Option<String>,
        }

        let args: Args = serde_json::from_value(args).unwrap_or_default();
        let use_local = args.timezone.as_deref() == Some("local");

        if use_local {
            let now: DateTime<Local> = Local::now();
            
            Ok(json!({
                "datetime": now.to_rfc3339(),
                "formatted": now.format("%A, %B %d, %Y at %I:%M %p").to_string(),
                "date": now.format("%Y-%m-%d").to_string(),
                "time": now.format("%H:%M:%S").to_string(),
                "timestamp": now.timestamp(),
                "timezone": "local"
            }))
        } else {
            let now: DateTime<Utc> = Utc::now();
            
            Ok(json!({
                "datetime": now.to_rfc3339(),
                "formatted": now.format("%A, %B %d, %Y at %I:%M %p UTC").to_string(),
                "date": now.format("%Y-%m-%d").to_string(),
                "time": now.format("%H:%M:%S").to_string(),
                "timestamp": now.timestamp(),
                "timezone": "UTC"
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_time_tool_name() {
        let tool = GetTimeTool::new();
        assert_eq!(tool.name(), "get_time");
    }

    #[test]
    fn test_get_time_tool_description() {
        let tool = GetTimeTool::new();
        assert!(tool.description().contains("time"));
        assert!(tool.description().contains("date"));
    }

    #[test]
    fn test_get_time_tool_schema() {
        let tool = GetTimeTool::new();
        let schema = tool.parameters_schema().unwrap();
        
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["timezone"].is_object());
    }

    #[test]
    fn test_get_time_tool_default() {
        let tool = GetTimeTool::default();
        assert_eq!(tool.name(), "get_time");
    }
}
