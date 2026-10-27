//! What a home claims of the UI protocols agents draw in (spec 3.3 §6.1 `genui`), in
//! adk-ui's claim vocabulary: each claim has a status and a scope, so a host never mistakes a
//! typed wire format for an end-to-end feature. Wire types of every `web` build.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What `GET /api/surface/genui/capabilities` answers: a table of claims per protocol, and
/// the catalogs this home renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenuiCapabilities {
    /// A2UI, AG-UI and MCP Apps, in that order.
    pub protocols: Vec<ProtocolClaims>,
    /// The catalogs this home renders.
    pub catalogs: Vec<CatalogView>,
}

/// One protocol's claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolClaims {
    /// Its key: `a2ui`, `ag_ui`, `mcp_apps`.
    pub id: String,
    /// Its name: `A2UI`, `AG-UI`, `MCP Apps`.
    pub name: String,
    /// What this home claims, in order.
    pub claims: Vec<Claim>,
}

/// One claim of a protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Claim {
    /// Its key, such as `version_0_9_1`.
    pub id: String,
    /// What it says: "Version 0.9.1", "No approval controls in drawn UI".
    pub label: String,
    /// How far the home supports it.
    pub status: ClaimStatus,
    /// Where it holds.
    pub scope: ClaimScope,
    /// A line on what it covers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How far a home supports a claim (adk-ui's vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStatus {
    /// End to end.
    Supported,
    /// In part; the note says which part.
    Partial,
    /// The host provides it.
    HostOwned,
    /// A release candidate of the protocol, not yet relied on.
    Candidate,
    /// Not yet.
    Planned,
    /// Kept for compatibility only.
    Legacy,
}

/// Where a claim holds (adk-ui's vocabulary).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClaimScope {
    /// The agent runtime.
    Runtime,
    /// The renderer.
    Client,
    /// The wire between them.
    Transport,
    /// The page hosting the renderer.
    Host,
    /// A guarantee to the person.
    Security,
}

/// A catalog this home renders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogView {
    /// Its id.
    pub id: String,
    /// Its name and version: `forms 1.0`.
    pub label: String,
    /// The components it renders.
    pub components: Vec<String>,
    /// The upstream components it leaves out, because they could fake a decision or cover
    /// the home.
    pub left_out: Vec<String>,
}
