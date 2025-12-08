//! Cookie management tools.

use crate::session::BrowserSession;
use adk_core::{AdkError, Result, Tool, ToolContext};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

/// Tool for getting all cookies.
pub struct GetCookiesTool {
    browser: Arc<BrowserSession>,
}

impl GetCookiesTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for GetCookiesTool {
    fn name(&self) -> &str {
        "browser_get_cookies"
    }

    fn description(&self) -> &str {
        "Get all cookies for the current page domain."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        let cookies = self.browser.get_all_cookies().await?;
        Ok(json!({
            "success": true,
            "cookies": cookies,
            "count": cookies.len()
        }))
    }
}

/// Tool for getting a specific cookie by name.
pub struct GetCookieTool {
    browser: Arc<BrowserSession>,
}

impl GetCookieTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for GetCookieTool {
    fn name(&self) -> &str {
        "browser_get_cookie"
    }

    fn description(&self) -> &str {
        "Get a specific cookie by name."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name of the cookie to retrieve"
                }
            },
            "required": ["name"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'name' parameter".to_string()))?;

        let cookie = self.browser.get_cookie(name).await?;
        Ok(json!({
            "success": true,
            "cookie": cookie
        }))
    }
}

/// Tool for adding a cookie.
pub struct AddCookieTool {
    browser: Arc<BrowserSession>,
}

impl AddCookieTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for AddCookieTool {
    fn name(&self) -> &str {
        "browser_add_cookie"
    }

    fn description(&self) -> &str {
        "Add a cookie to the browser."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Cookie name"
                },
                "value": {
                    "type": "string",
                    "description": "Cookie value"
                },
                "domain": {
                    "type": "string",
                    "description": "Cookie domain (optional)"
                },
                "path": {
                    "type": "string",
                    "description": "Cookie path (default: '/')"
                },
                "secure": {
                    "type": "boolean",
                    "description": "Secure flag (default: false)"
                },
                "expiry": {
                    "type": "integer",
                    "description": "Expiry timestamp in seconds since epoch"
                }
            },
            "required": ["name", "value"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'name' parameter".to_string()))?;

        let value = args
            .get("value")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'value' parameter".to_string()))?;

        let domain = args.get("domain").and_then(|v| v.as_str());
        let path = args.get("path").and_then(|v| v.as_str());
        let secure = args.get("secure").and_then(|v| v.as_bool());
        let expiry = args.get("expiry").and_then(|v| v.as_i64());

        self.browser.add_cookie(name, value, domain, path, secure, expiry).await?;

        Ok(json!({
            "success": true,
            "added_cookie": name
        }))
    }
}

/// Tool for deleting a cookie.
pub struct DeleteCookieTool {
    browser: Arc<BrowserSession>,
}

impl DeleteCookieTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for DeleteCookieTool {
    fn name(&self) -> &str {
        "browser_delete_cookie"
    }

    fn description(&self) -> &str {
        "Delete a specific cookie by name."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name of the cookie to delete"
                }
            },
            "required": ["name"]
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value> {
        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AdkError::Tool("Missing 'name' parameter".to_string()))?;

        self.browser.delete_cookie(name).await?;

        Ok(json!({
            "success": true,
            "deleted_cookie": name
        }))
    }
}

/// Tool for deleting all cookies.
pub struct DeleteAllCookiesTool {
    browser: Arc<BrowserSession>,
}

impl DeleteAllCookiesTool {
    pub fn new(browser: Arc<BrowserSession>) -> Self {
        Self { browser }
    }
}

#[async_trait]
impl Tool for DeleteAllCookiesTool {
    fn name(&self) -> &str {
        "browser_delete_all_cookies"
    }

    fn description(&self) -> &str {
        "Delete all cookies for the current domain."
    }

    fn parameters_schema(&self) -> Option<Value> {
        Some(json!({
            "type": "object",
            "properties": {}
        }))
    }

    async fn execute(&self, _ctx: Arc<dyn ToolContext>, _args: Value) -> Result<Value> {
        self.browser.delete_all_cookies().await?;

        Ok(json!({
            "success": true,
            "message": "All cookies deleted"
        }))
    }
}
