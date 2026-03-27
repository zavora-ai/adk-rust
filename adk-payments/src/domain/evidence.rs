use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Identifies the protocol and version that originated canonical data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolDescriptor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl ProtocolDescriptor {
    /// Creates a protocol descriptor for ACP.
    #[must_use]
    pub fn acp(version: impl Into<String>) -> Self {
        Self { name: "acp".to_string(), version: Some(version.into()) }
    }

    /// Creates a protocol descriptor for AP2.
    #[must_use]
    pub fn ap2(version: impl Into<String>) -> Self {
        Self { name: "ap2".to_string(), version: Some(version.into()) }
    }

    /// Creates a protocol descriptor for any named protocol.
    #[must_use]
    pub fn new(name: impl Into<String>, version: Option<String>) -> Self {
        Self { name: name.into(), version }
    }
}

/// Opaque reference to a stored evidence artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceReference {
    pub evidence_id: String,
    pub protocol: ProtocolDescriptor,
    pub artifact_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

/// Lossless protocol-specific fields preserved alongside canonical state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolExtensionEnvelope {
    pub protocol: ProtocolDescriptor,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub fields: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<EvidenceReference>,
}

impl ProtocolExtensionEnvelope {
    /// Creates an empty extension envelope for one protocol source.
    #[must_use]
    pub fn new(protocol: ProtocolDescriptor) -> Self {
        Self { protocol, fields: Map::new(), evidence_refs: Vec::new() }
    }

    /// Appends one raw protocol field without discarding its original shape.
    #[must_use]
    pub fn with_field(mut self, key: impl Into<String>, value: Value) -> Self {
        self.fields.insert(key.into(), value);
        self
    }

    /// Appends one evidence reference associated with the extension envelope.
    #[must_use]
    pub fn with_evidence_ref(mut self, evidence_ref: EvidenceReference) -> Self {
        self.evidence_refs.push(evidence_ref);
        self
    }

    /// Returns `true` when the envelope carries neither fields nor evidence
    /// references.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.evidence_refs.is_empty()
    }
}

/// Collection of preserved protocol extension envelopes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProtocolExtensions(pub Vec<ProtocolExtensionEnvelope>);

impl ProtocolExtensions {
    /// Returns `true` when no protocol-specific envelopes are attached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Adds one protocol envelope to the collection.
    pub fn push(&mut self, envelope: ProtocolExtensionEnvelope) {
        self.0.push(envelope);
    }

    /// Returns all attached envelopes.
    #[must_use]
    pub fn as_slice(&self) -> &[ProtocolExtensionEnvelope] {
        &self.0
    }
}

impl From<Vec<ProtocolExtensionEnvelope>> for ProtocolExtensions {
    fn from(value: Vec<ProtocolExtensionEnvelope>) -> Self {
        Self(value)
    }
}
