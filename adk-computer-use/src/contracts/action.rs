//! Action classification, resource context, provenance, postconditions, and the
//! immutable [`ActionEnvelope`] proposed by the graph and enforced by the runtime.

use super::target::{TargetEvidence, TargetSensitivityEvidence};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;

/// Enforced execution mode selected for an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Observe and preview only.
    Shadow,
    /// Proved non-foreground actuation only.
    Background,
    /// Bounded exclusive foreground transaction.
    Foreground,
}

/// Operation-aware action class used by authorization and policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    /// Read-only observation.
    Observe,
    /// Navigation without persistent side effects.
    Navigate,
    /// A reversible edit.
    EditReversible,
    /// Communication that leaves the machine.
    CommunicateExternal,
    /// An authentication interaction.
    Authentication,
    /// A financial transaction.
    Financial,
    /// A destructive, non-reversible operation.
    Destructive,
    /// A change to privileges or permissions.
    PrivilegeChange,
    /// Access to secret material.
    SecretAccess,
}

/// Optional resource identifiers describing what an action touches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActionResourceContext {
    /// Target application/bundle identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_app_id: Option<String>,
    /// Target window identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_window_id: Option<Value>,
    /// Filesystem path acted upon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesystem_path: Option<String>,
    /// Filesystem destination (e.g. for a move/copy).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filesystem_destination: Option<String>,
    /// Registry path acted upon (Windows).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry_path: Option<String>,
    /// Process name acted upon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_name: Option<String>,
    /// Process identifier acted upon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_id: Option<u32>,
    /// Browser domain acted upon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_domain: Option<String>,
}

/// Provenance describing whether an action derives from untrusted instructions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionProvenance {
    /// Whether the action originates from untrusted instruction text.
    pub untrusted_instruction: bool,
    /// Observation frames the action was derived from.
    pub source_observation_ids: Vec<String>,
    /// Whether the action crosses a data trust boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crosses_data_boundary: Option<bool>,
}

fn deserialize_false<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let value = bool::deserialize(deserializer)?;
    if value {
        return Err(de::Error::custom(
            "process postcondition can only prove a non-running process",
        ));
    }
    Ok(false)
}

fn serialize_false<S>(value: &bool, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if *value {
        return Err(serde::ser::Error::custom(
            "process postcondition can only prove a non-running process",
        ));
    }
    serializer.serialize_bool(false)
}

/// Digest-only expected state independently verified by computer-use-mcp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ActionPostcondition {
    /// Expected UI element state, keyed by role/label with a value digest.
    #[serde(rename = "ui_element")]
    UiElement {
        /// Accessibility role of the element, if specified.
        #[serde(skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        /// Label of the element, if specified.
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Whether the element is expected to exist.
        exists: bool,
        /// Digest of the expected value (never the raw value).
        #[serde(rename = "valueDigest", skip_serializing_if = "Option::is_none")]
        value_digest: Option<String>,
    },
    /// Expected filesystem state with a content digest.
    #[serde(rename = "filesystem")]
    Filesystem {
        /// Path expected to exist or not.
        path: String,
        /// Whether the path is expected to exist.
        exists: bool,
        /// Digest of the expected file contents (never the raw contents).
        #[serde(rename = "contentDigest", skip_serializing_if = "Option::is_none")]
        content_digest: Option<String>,
    },
    /// Expected registry state with a value digest (Windows).
    #[serde(rename = "registry")]
    Registry {
        /// Registry path.
        path: String,
        /// Value name.
        name: String,
        /// Whether the value is expected to exist.
        exists: bool,
        /// Digest of the expected value (never the raw value).
        #[serde(rename = "valueDigest", skip_serializing_if = "Option::is_none")]
        value_digest: Option<String>,
    },
    /// Expected process state. Can only prove a process is *not* running.
    #[serde(rename = "process")]
    Process {
        /// Process identifier.
        pid: u32,
        /// Must be `false`; a running-process assertion is rejected on the wire.
        #[serde(deserialize_with = "deserialize_false", serialize_with = "serialize_false")]
        running: bool,
    },
    /// Expected window existence state.
    #[serde(rename = "window")]
    Window {
        /// Window identifier.
        #[serde(rename = "windowId")]
        window_id: u64,
        /// Whether the window is expected to exist.
        exists: bool,
    },
}

/// Immutable action proposed by the graph and enforced by the runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionEnvelope {
    /// Unique action identifier.
    pub action_id: String,
    /// The session this action belongs to.
    pub session_id: String,
    /// Optional execution group for multi-agent coordination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_group_id: Option<String>,
    /// Authenticated principal proposing the action.
    pub principal_id: String,
    /// Optional agent identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Runtime tool name.
    pub tool: String,
    /// Semantic operation name.
    pub operation: String,
    /// Operation-aware action class.
    pub action_class: ActionClass,
    /// Requested execution mode.
    pub requested_mode: ExecutionMode,
    /// Fresh target evidence, when the action has a target.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetEvidence>,
    /// Value-free target sensitivity evidence, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_sensitivity: Option<TargetSensitivityEvidence>,
    /// Resource identifiers the action touches.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<ActionResourceContext>,
    /// Provenance describing instruction trust.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ActionProvenance>,
    /// Data sensitivity labels attached to the action.
    pub data_labels: Vec<String>,
    /// Digest-only postcondition independently verified by the runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub postcondition: Option<ActionPostcondition>,
    /// Whether the action is reversible.
    pub reversible: bool,
    /// Whether the action has an external side effect.
    pub external_side_effect: bool,
    /// RFC 3339 timestamp the action was proposed.
    pub proposed_at: String,
    /// RFC 3339 expiry after which the action is invalid.
    pub expires_at: String,
    /// Digest of the action arguments, bound by approval.
    pub args_digest: String,
}
