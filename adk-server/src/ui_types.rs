//! MCP Apps render-option types and validation.
//!
//! Inlined from `adk-ui::interop::mcp_apps` so `adk-server` can validate
//! UI resource registrations without depending on the full UI toolkit.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpUiResourceCsp {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connect_domains: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_domains: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_domains: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_uri_domains: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct PermissionGrant {}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct CapabilityGrant {}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpUiPermissions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera: Option<PermissionGrant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub microphone: Option<PermissionGrant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geolocation: Option<PermissionGrant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clipboard_write: Option<PermissionGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpUiContentCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<CapabilityGrant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<CapabilityGrant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<CapabilityGrant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_link: Option<CapabilityGrant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<CapabilityGrant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<CapabilityGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpUiListChangedCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpUiSandboxCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csp: Option<McpUiResourceCsp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<McpUiPermissions>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpUiHostCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<McpUiContentCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_model_context: Option<McpUiContentCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_links: Option<CapabilityGrant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_file: Option<CapabilityGrant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<CapabilityGrant>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_resources: Option<McpUiListChangedCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_tools: Option<McpUiListChangedCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<McpUiSandboxCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpUiHostInfo {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpUiBridgeSnapshot {
    pub protocol_version: String,
    pub initialized: bool,
    pub host_info: McpUiHostInfo,
    pub host_capabilities: McpUiHostCapabilities,
    pub host_context: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_capabilities: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_info: Option<Value>,
}

impl McpUiBridgeSnapshot {
    pub fn new(
        protocol_version: impl Into<String>,
        initialized: bool,
        host_info: McpUiHostInfo,
        host_capabilities: McpUiHostCapabilities,
        host_context: Value,
    ) -> Self {
        Self {
            protocol_version: protocol_version.into(),
            initialized,
            host_info,
            host_capabilities,
            host_context,
            app_capabilities: None,
            app_info: None,
        }
    }

    pub fn with_app_metadata(mut self, app_info: Value, app_capabilities: Value) -> Self {
        self.app_info = Some(app_info);
        self.app_capabilities = Some(app_capabilities);
        self
    }

    pub fn with_optional_app_metadata(
        mut self,
        app_info: Option<Value>,
        app_capabilities: Option<Value>,
    ) -> Self {
        self.app_info = app_info;
        self.app_capabilities = app_capabilities;
        self
    }

    pub fn into_tool_result_bridge(self) -> McpUiToolResultBridge {
        McpUiToolResultBridge {
            protocol_version: Some(self.protocol_version),
            structured_content: None,
            host_info: Some(self.host_info),
            host_capabilities: Some(self.host_capabilities),
            host_context: Some(self.host_context),
            app_capabilities: self.app_capabilities,
            app_info: self.app_info,
            initialized: Some(self.initialized),
        }
    }

    pub fn build_tool_result(
        self,
        structured_content: Option<Value>,
        resource_uri: Option<String>,
        html: Option<String>,
    ) -> McpUiToolResult {
        let mut bridge = self.into_tool_result_bridge();
        if let Some(structured_content) = structured_content {
            bridge = bridge.with_structured_content(structured_content);
        }

        let mut result = McpUiToolResult::default().with_bridge(bridge);
        if let Some(resource_uri) = resource_uri {
            result = result.with_resource_uri(resource_uri);
        }
        if let Some(html) = html {
            result = result.with_html(html);
        }
        result
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpUiToolResultBridge {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_info: Option<McpUiHostInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_capabilities: Option<McpUiHostCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_context: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_capabilities: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_info: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initialized: Option<bool>,
}

impl McpUiToolResultBridge {
    pub fn from_host_bridge(
        protocol_version: impl Into<String>,
        initialized: bool,
        host_info: McpUiHostInfo,
        host_capabilities: McpUiHostCapabilities,
        host_context: Value,
    ) -> Self {
        McpUiBridgeSnapshot::new(
            protocol_version,
            initialized,
            host_info,
            host_capabilities,
            host_context,
        )
        .into_tool_result_bridge()
    }

    pub fn with_structured_content(mut self, structured_content: Value) -> Self {
        self.structured_content = Some(structured_content);
        self
    }

    pub fn with_app_metadata(mut self, app_info: Value, app_capabilities: Value) -> Self {
        self.app_info = Some(app_info);
        self.app_capabilities = Some(app_capabilities);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct McpUiToolResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bridge: Option<McpUiToolResultBridge>,
}

impl McpUiToolResult {
    pub fn with_resource_uri(mut self, resource_uri: impl Into<String>) -> Self {
        self.resource_uri = Some(resource_uri.into());
        self
    }

    pub fn with_html(mut self, html: impl Into<String>) -> Self {
        self.html = Some(html.into());
        self
    }

    pub fn with_bridge(mut self, bridge: McpUiToolResultBridge) -> Self {
        self.bridge = Some(bridge);
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpAppsRenderOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefers_border: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csp: Option<McpUiResourceCsp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<McpUiPermissions>,
}

fn is_allowed_domain(domain: &str) -> bool {
    domain.starts_with("https://")
        || domain.starts_with("http://localhost")
        || domain.starts_with("http://127.0.0.1")
}

fn validate_domain_list(domains: Option<&Vec<String>>, field: &str) -> Result<(), String> {
    let Some(domains) = domains else {
        return Ok(());
    };
    for domain in domains {
        if !is_allowed_domain(domain) {
            return Err(format!(
                "Invalid mcp_apps option '{}': unsupported domain '{}'",
                field, domain
            ));
        }
    }
    Ok(())
}

pub fn validate_mcp_apps_render_options(options: &McpAppsRenderOptions) -> Result<(), String> {
    if let Some(domain) = options.domain.as_deref() {
        if !is_allowed_domain(domain) {
            return Err(format!(
                "Invalid mcp_apps option 'domain': unsupported domain '{}'",
                domain
            ));
        }
    }
    if let Some(csp) = &options.csp {
        validate_domain_list(csp.connect_domains.as_ref(), "csp.connect_domains")?;
        validate_domain_list(csp.resource_domains.as_ref(), "csp.resource_domains")?;
        validate_domain_list(csp.frame_domains.as_ref(), "csp.frame_domains")?;
        validate_domain_list(csp.base_uri_domains.as_ref(), "csp.base_uri_domains")?;
    }
    Ok(())
}

pub fn default_mcp_ui_host_info() -> McpUiHostInfo {
    McpUiHostInfo {
        name: "adk-server".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        title: Some("ADK Server".to_string()),
        description: Some(
            "Additive HTTP bridge for MCP Apps initialize/message/model-context flows plus lifecycle notifications.".to_string(),
        ),
        website_url: None,
    }
}

pub fn default_mcp_ui_host_capabilities() -> McpUiHostCapabilities {
    let supported_content = McpUiContentCapabilities {
        text: Some(CapabilityGrant::default()),
        structured_content: Some(CapabilityGrant::default()),
        resource: Some(CapabilityGrant::default()),
        resource_link: Some(CapabilityGrant::default()),
        image: None,
        audio: None,
    };

    McpUiHostCapabilities {
        message: Some(supported_content.clone()),
        update_model_context: Some(supported_content),
        open_links: None,
        download_file: None,
        logging: None,
        server_resources: Some(McpUiListChangedCapability { list_changed: Some(true) }),
        server_tools: Some(McpUiListChangedCapability { list_changed: Some(true) }),
        sandbox: Some(McpUiSandboxCapabilities::default()),
        experimental: None,
    }
}
