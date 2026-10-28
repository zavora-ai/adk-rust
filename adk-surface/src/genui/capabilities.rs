//! This home's claims (spec 3.3 §6.1 `genui`): A2UI v0.9.1 on the `forms` 1.0 catalog, with
//! no approval controls in anything drawn; drawings over AG-UI; and, with `genui-apps`, a
//! governed MCP Apps host.

use super::capabilities_wire::{
    CatalogView, Claim, ClaimScope, ClaimStatus, GenuiCapabilities, ProtocolClaims,
};
use super::catalog::Catalog;

/// The note of a claim this build supports that the server did not wire.
const NOT_WIRED: &str = "Not set on this server";

/// What a host wired beside the agent-drawn UI routes (spec 3.3 §6.1 `genui`): a claim of
/// AG-UI or MCP Apps holds only where it is wired.
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::GenuiWiring;
///
/// let wiring = GenuiWiring { ag_ui: true, ..GenuiWiring::default() };
/// assert!(!wiring.mcp_apps);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GenuiWiring {
    /// `GenuiAgUi` is set with `ServerBuilder::with_ag_ui_extension`.
    pub ag_ui: bool,
    /// An `McpAppsHost` is mounted with `web::genui::apps_router` (`genui-apps`).
    pub mcp_apps: bool,
}

/// `claim`, or planned with the note that the server did not wire it.
fn wired(claim: Claim, wired: bool) -> Claim {
    if wired {
        return claim;
    }
    Claim { status: ClaimStatus::Planned, note: Some(NOT_WIRED.to_string()), ..claim }
}

/// What this home claims of MCP Apps: a governed host with `genui-apps` where it is mounted,
/// nothing yet without.
fn mcp_apps(mounted: bool) -> Vec<Claim> {
    use ClaimScope::{Client, Host, Security};
    use ClaimStatus::Planned;
    #[cfg(feature = "genui-apps")]
    let (version, sandbox, governed) =
        (ClaimStatus::Supported, ClaimStatus::HostOwned, ClaimStatus::Supported);
    #[cfg(not(feature = "genui-apps"))]
    let (version, sandbox, governed) = (Planned, Planned, Planned);
    let mounted = mounted || !cfg!(feature = "genui-apps");
    vec![
        wired(claim("version_2026_01_26", "Version 2026-01-26", version, Host), mounted),
        wired(claim("sandbox_and_csp", "Sandbox and CSP", sandbox, Host), mounted),
        wired(
            claim("app_tool_calls_governed", "App tool calls governed", governed, Security),
            mounted,
        ),
        claim("fullscreen", "Fullscreen and picture-in-picture", Planned, Client),
    ]
}

/// The upstream basic catalog's components `forms` 1.0 leaves out.
const LEFT_OUT: &[&str] = &["Image", "Icon", "Modal", "Video", "AudioPlayer"];

fn claim(id: &str, label: &str, status: ClaimStatus, scope: ClaimScope) -> Claim {
    Claim { id: id.to_string(), label: label.to_string(), status, scope, note: None }
}

/// What a home with nothing wired beside the routes claims of the UI protocols agents draw
/// in, in adk-ui's vocabulary (spec 3.3 §6.1 `genui`): A2UI, with AG-UI and MCP Apps "Not set
/// on this server"; [`capabilities_wired`] for what a host wired.
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
    capabilities_wired(GenuiWiring::default())
}

/// What this home claims with `wiring`: the AG-UI claims where `GenuiAgUi` is set, the MCP
/// Apps claims where an apps host is mounted (spec 3.3 §6.1 `genui`, brief 13 "only when
/// wired").
///
/// # Example
///
/// ```rust
/// use adk_surface::genui::{ClaimStatus, GenuiWiring, capabilities, capabilities_wired};
///
/// let ag_ui = |claims: adk_surface::genui::GenuiCapabilities| claims.protocols[1].claims[0].status;
/// assert_eq!(ag_ui(capabilities()), ClaimStatus::Planned);
/// let wired = capabilities_wired(GenuiWiring { ag_ui: true, mcp_apps: false });
/// assert_eq!(ag_ui(wired), ClaimStatus::Supported);
/// ```
pub fn capabilities_wired(wiring: GenuiWiring) -> GenuiCapabilities {
    use ClaimScope::{Runtime, Security, Transport};
    use ClaimStatus::{Candidate, Supported};
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
                    wired(
                        claim(
                            "a2ui_surface_activity",
                            "a2ui-surface activity",
                            Supported,
                            Transport,
                        ),
                        wiring.ag_ui,
                    ),
                    wired(
                        claim(
                            "drawn_submit_interrupts",
                            "Interrupts for a drawn submit",
                            Supported,
                            Transport,
                        ),
                        wiring.ag_ui,
                    ),
                ],
            },
            ProtocolClaims {
                id: "mcp_apps".into(),
                name: "MCP Apps".into(),
                claims: mcp_apps(wiring.mcp_apps),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that mounts the routes without the AG-UI extension or an apps host claims
    /// neither (brief 13: "only when wired").
    #[test]
    fn what_a_server_did_not_wire_is_not_claimed() {
        let claims = capabilities();
        let unwired: Vec<(String, ClaimStatus, Option<String>)> = claims.protocols[1..]
            .iter()
            .flat_map(|protocol| &protocol.claims)
            .filter(|claim| claim.id != "fullscreen")
            .map(|claim| (claim.id.clone(), claim.status, claim.note.clone()))
            .collect();
        assert!(
            unwired.iter().all(|(_, status, _)| *status == ClaimStatus::Planned),
            "{unwired:?}"
        );
        assert_eq!(claims.protocols[0].claims[0].status, ClaimStatus::Supported);
    }
}
