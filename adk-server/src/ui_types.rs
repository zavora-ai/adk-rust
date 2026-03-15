//! MCP Apps render-option types and validation.
//!
//! Inlined from `adk-ui::interop::mcp_apps` so `adk-server` can validate
//! UI resource registrations without depending on the full UI toolkit.

use serde::{Deserialize, Serialize};

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
