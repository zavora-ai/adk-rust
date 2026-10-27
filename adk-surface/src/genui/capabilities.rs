//! This home's claims (spec 3.3 §6.1 `genui`): A2UI v0.9.1 on the `forms` 1.0 catalog, with
//! no approval controls in anything drawn. AG-UI and MCP Apps come with `genui-apps`.

use super::capabilities_wire::{
    CatalogView, Claim, ClaimScope, ClaimStatus, GenuiCapabilities, ProtocolClaims,
};
use super::catalog::Catalog;

/// The upstream basic catalog's components `forms` 1.0 leaves out.
const LEFT_OUT: &[&str] = &["Image", "Icon", "Modal", "Video", "AudioPlayer"];

fn claim(id: &str, label: &str, status: ClaimStatus, scope: ClaimScope) -> Claim {
    Claim { id: id.to_string(), label: label.to_string(), status, scope, note: None }
}

/// What this home claims of the UI protocols agents draw in, in adk-ui's vocabulary
/// (spec 3.3 §6.1 `genui`).
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{ClaimStatus, capabilities};
///
/// let claims = capabilities();
/// let a2ui = &claims.protocols[0];
/// assert_eq!(a2ui.id, "a2ui");
/// assert_eq!(a2ui.claims[0].label, "Version 0.9.1");
/// assert_eq!(a2ui.claims[0].status, ClaimStatus::Supported);
/// assert_eq!(claims.catalogs[0].label, "forms 1.0");
/// ```
pub fn capabilities() -> GenuiCapabilities {
    use ClaimScope::{Client, Host, Runtime, Security, Transport};
    use ClaimStatus::{Candidate, Planned, Supported};
    let forms = Catalog::forms();
    let mut components: Vec<String> = forms.components().map(str::to_string).collect();
    components.sort();
    GenuiCapabilities {
        protocols: vec![
            ProtocolClaims {
                id: "a2ui".into(),
                name: "A2UI".into(),
                claims: vec![
                    claim("version_0_9_1", "Version 0.9.1", Supported, Runtime),
                    claim("version_1_0", "Version 1.0", Candidate, Runtime),
                    claim("trusted_catalog", "Trusted component catalog", Supported, Security),
                    claim(
                        "no_approval_controls",
                        "No approval controls in drawn UI",
                        Supported,
                        Security,
                    ),
                ],
            },
            ProtocolClaims {
                id: "ag_ui".into(),
                name: "AG-UI".into(),
                claims: vec![
                    claim("a2ui_surface_activity", "a2ui-surface activity", Planned, Transport),
                    claim(
                        "drawn_submit_interrupts",
                        "Interrupts for a drawn submit",
                        Planned,
                        Transport,
                    ),
                ],
            },
            ProtocolClaims {
                id: "mcp_apps".into(),
                name: "MCP Apps".into(),
                claims: vec![
                    claim("version_2026_01_26", "Version 2026-01-26", Planned, Host),
                    claim("sandbox_and_csp", "Sandbox and CSP", Planned, Host),
                    claim("app_tool_calls_governed", "App tool calls governed", Planned, Security),
                    claim("fullscreen", "Fullscreen and picture-in-picture", Planned, Client),
                ],
            },
        ],
        catalogs: vec![CatalogView {
            id: forms.id().to_string(),
            label: forms.label(),
            components,
            left_out: LEFT_OUT.iter().map(|name| (*name).to_string()).collect(),
        }],
    }
}
