//! Agent identity: who an agent is, who sponsors it, and on whose behalf it acts.
//!
//! | Type | Identifies |
//! |------|------------|
//! | [`AgentId`] | One agent, stable across redeploys and replicas |
//! | [`SubjectRef`] | A human, a service, or an agent, by kind and identifier |
//! | [`AgentPrincipal`] | An agent with its tenant, class, instance, sponsor, and attributes |
//! | [`ActingContext`] | An agent acting in one invocation, on behalf of a subject, through a delegation chain |
//!
//! Every type serializes with camelCase field names, so a principal recorded by one
//! process (in a run record, an audit entry, or a governance state) reads back unchanged
//! in another.
//!
//! # Example
//!
//! ```rust
//! use adk_core::principal::{ActingContext, AgentId, AgentPrincipal, SubjectRef};
//!
//! let agent = AgentPrincipal::new(AgentId::new("urn:adk:agent:billing")?)
//!     .with_tenant("acme")
//!     .with_sponsor(SubjectRef::human("alice@example.com"));
//! let acting = ActingContext::new(agent, "inv-1")
//!     .on_behalf_of(SubjectRef::human("bob@example.com"));
//!
//! let json = serde_json::to_value(&acting)?;
//! assert_eq!(json["agent"]["id"], "urn:adk:agent:billing");
//! assert_eq!(json["onBehalfOf"]["kind"], "human");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum length of an [`AgentId`], in bytes.
pub const MAX_AGENT_ID_LEN: usize = 512;

/// Error returned when a string is not a valid [`AgentId`].
///
/// # Example
///
/// ```rust
/// use adk_core::principal::{AgentId, PrincipalError};
///
/// assert_eq!(AgentId::new(""), Err(PrincipalError::Empty));
/// ```
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PrincipalError {
    /// The agent identifier was empty.
    #[error("agent id must not be empty; use a URI such as `urn:adk:agent:<name>`")]
    Empty,
    /// The agent identifier exceeded [`MAX_AGENT_ID_LEN`].
    #[error(
        "agent id is {len} bytes, more than the {MAX_AGENT_ID_LEN}-byte limit; use a shorter URI"
    )]
    TooLong {
        /// The length of the rejected identifier, in bytes.
        len: usize,
    },
    /// The agent identifier contained a control character.
    #[error("agent id must not contain control characters (found U+{code_point:04X})")]
    ContainsControl {
        /// The first control character found.
        code_point: u32,
    },
}

/// Opaque, stable agent identifier.
///
/// A URI such as `spiffe://example.org/agent/billing` or `urn:adk:agent:billing`. It
/// survives redeploys and is shared by every replica of the agent. Validation requires
/// a non-empty value of at most [`MAX_AGENT_ID_LEN`] bytes with no control characters;
/// the URI scheme is not checked, so any identity system's naming fits.
///
/// Deserialization applies the same validation.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::AgentId;
///
/// let id: AgentId = "spiffe://example.org/agent/billing".parse()?;
/// assert_eq!(id.as_str(), "spiffe://example.org/agent/billing");
/// assert!(AgentId::new("bad\nid").is_err());
/// # Ok::<(), adk_core::principal::PrincipalError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentId(String);

impl AgentId {
    /// Creates an agent identifier after validating it.
    ///
    /// # Errors
    ///
    /// Returns [`PrincipalError`] when the value is empty, longer than
    /// [`MAX_AGENT_ID_LEN`] bytes, or contains a control character.
    pub fn new(value: impl Into<String>) -> Result<Self, PrincipalError> {
        let value = value.into();
        if value.is_empty() {
            return Err(PrincipalError::Empty);
        }
        if value.len() > MAX_AGENT_ID_LEN {
            return Err(PrincipalError::TooLong { len: value.len() });
        }
        if let Some(control) = value.chars().find(|c| c.is_control()) {
            return Err(PrincipalError::ContainsControl { code_point: control as u32 });
        }
        Ok(Self(value))
    }

    /// Returns the identifier as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for AgentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for AgentId {
    type Err = PrincipalError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for AgentId {
    type Error = PrincipalError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for AgentId {
    type Error = PrincipalError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

/// What kind of subject a [`SubjectRef`] names.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::SubjectKind;
///
/// assert_eq!(serde_json::to_value(SubjectKind::Service).unwrap(), "service");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum SubjectKind {
    /// A person.
    Human,
    /// A non-agent workload, such as a backend service or a scheduler.
    Service,
    /// An agent; the identifier is an [`AgentId`] string.
    Agent,
}

/// A reference to a human, a service, or an agent.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::{SubjectKind, SubjectRef};
///
/// let alice = SubjectRef::human("alice@example.com");
/// assert_eq!(alice, SubjectRef { kind: SubjectKind::Human, id: "alice@example.com".into() });
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectRef {
    /// The kind of subject.
    pub kind: SubjectKind,
    /// The subject's identifier in its identity system.
    pub id: String,
}

impl SubjectRef {
    /// Creates a reference to a subject of `kind`.
    pub fn new(kind: SubjectKind, id: impl Into<String>) -> Self {
        Self { kind, id: id.into() }
    }

    /// Creates a reference to a person.
    pub fn human(id: impl Into<String>) -> Self {
        Self::new(SubjectKind::Human, id)
    }

    /// Creates a reference to a non-agent workload.
    pub fn service(id: impl Into<String>) -> Self {
        Self::new(SubjectKind::Service, id)
    }

    /// Creates a reference to an agent.
    pub fn agent(id: &AgentId) -> Self {
        Self::new(SubjectKind::Agent, id.as_str())
    }
}

/// An agent's identity: its stable identifier plus where and for whom it runs.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::{AgentId, AgentPrincipal, SubjectRef};
///
/// let principal = AgentPrincipal::new(AgentId::new("urn:adk:agent:billing")?)
///     .with_tenant("acme")
///     .with_class("billing-v2")
///     .with_instance("replica-3")
///     .with_sponsor(SubjectRef::human("alice@example.com"))
///     .with_attribute("cost-center", "finance");
/// assert_eq!(principal.attributes["cost-center"], "finance");
/// # Ok::<(), adk_core::principal::PrincipalError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPrincipal {
    /// The agent's stable identifier.
    pub id: AgentId,
    /// The tenant the agent belongs to, in a multi-tenant deployment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    /// The template or blueprint the agent was created from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
    /// The deployment or replica running the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// The subject accountable for the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sponsor: Option<SubjectRef>,
    /// Further attributes, ordered by key so serialization is deterministic.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

impl AgentPrincipal {
    /// Creates a principal with only an identifier.
    pub fn new(id: AgentId) -> Self {
        Self {
            id,
            tenant: None,
            class: None,
            instance: None,
            sponsor: None,
            attributes: BTreeMap::new(),
        }
    }

    /// Sets the tenant.
    pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = Some(tenant.into());
        self
    }

    /// Sets the class (template or blueprint).
    pub fn with_class(mut self, class: impl Into<String>) -> Self {
        self.class = Some(class.into());
        self
    }

    /// Sets the instance (deployment or replica).
    pub fn with_instance(mut self, instance: impl Into<String>) -> Self {
        self.instance = Some(instance.into());
        self
    }

    /// Sets the sponsor.
    pub fn with_sponsor(mut self, sponsor: SubjectRef) -> Self {
        self.sponsor = Some(sponsor);
        self
    }

    /// Adds or replaces one attribute.
    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }
}

/// An agent acting in one invocation.
///
/// `delegation_chain` lists the subjects that handed the work down, outermost first:
/// a human who asked an orchestrator agent, which delegated to this agent, reads
/// `[human, orchestrator]`.
///
/// # Example
///
/// ```rust
/// use adk_core::principal::{ActingContext, AgentId, AgentPrincipal, SubjectRef};
///
/// let orchestrator = AgentId::new("urn:adk:agent:orchestrator")?;
/// let acting = ActingContext::new(
///     AgentPrincipal::new(AgentId::new("urn:adk:agent:billing")?),
///     "inv-42",
/// )
/// .on_behalf_of(SubjectRef::human("bob@example.com"))
/// .delegated_by(SubjectRef::human("bob@example.com"))
/// .delegated_by(SubjectRef::agent(&orchestrator));
/// assert_eq!(acting.delegation_chain.len(), 2);
/// # Ok::<(), adk_core::principal::PrincipalError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActingContext {
    /// The acting agent.
    pub agent: AgentPrincipal,
    /// The subject the agent acts for, when it acts for one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<SubjectRef>,
    /// The subjects that delegated the work, outermost first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delegation_chain: Vec<SubjectRef>,
    /// The invocation the agent acts in.
    pub invocation_id: String,
}

impl ActingContext {
    /// Creates a context for `agent` acting in `invocation_id`.
    pub fn new(agent: AgentPrincipal, invocation_id: impl Into<String>) -> Self {
        Self {
            agent,
            on_behalf_of: None,
            delegation_chain: Vec::new(),
            invocation_id: invocation_id.into(),
        }
    }

    /// Sets the subject the agent acts for.
    pub fn on_behalf_of(mut self, subject: SubjectRef) -> Self {
        self.on_behalf_of = Some(subject);
        self
    }

    /// Appends a subject to the end of the delegation chain.
    pub fn delegated_by(mut self, subject: SubjectRef) -> Self {
        self.delegation_chain.push(subject);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_id_validation() {
        assert_eq!(AgentId::new(""), Err(PrincipalError::Empty));
        assert_eq!(
            AgentId::new("a".repeat(MAX_AGENT_ID_LEN + 1)),
            Err(PrincipalError::TooLong { len: MAX_AGENT_ID_LEN + 1 })
        );
        assert_eq!(
            AgentId::new("urn:adk:\u{7f}"),
            Err(PrincipalError::ContainsControl { code_point: 0x7f })
        );
        assert_eq!(AgentId::new("x\0y"), Err(PrincipalError::ContainsControl { code_point: 0 }));
        assert!(AgentId::new("a".repeat(MAX_AGENT_ID_LEN)).is_ok());
        assert!(AgentId::new("urn:adk:agent:fakturering/ø").is_ok());
    }

    #[test]
    fn agent_id_deserialization_validates() {
        let parsed: AgentId = serde_json::from_value(json!("urn:adk:agent:a")).unwrap();
        assert_eq!(parsed.as_str(), "urn:adk:agent:a");
        assert!(serde_json::from_value::<AgentId>(json!("")).is_err());
        assert!(serde_json::from_value::<AgentId>(json!("a\tb")).is_err());
    }

    #[test]
    fn acting_context_round_trips_in_camel_case() {
        let acting = ActingContext::new(
            AgentPrincipal::new(AgentId::new("spiffe://example.org/agent/billing").unwrap())
                .with_tenant("acme")
                .with_class("billing")
                .with_instance("r1")
                .with_sponsor(SubjectRef::human("alice"))
                .with_attribute("tier", "gold"),
            "inv-1",
        )
        .on_behalf_of(SubjectRef::service("crm"))
        .delegated_by(SubjectRef::human("alice"));

        let value = serde_json::to_value(&acting).unwrap();
        assert_eq!(
            value,
            json!({
                "agent": {
                    "id": "spiffe://example.org/agent/billing",
                    "tenant": "acme",
                    "class": "billing",
                    "instance": "r1",
                    "sponsor": {"kind": "human", "id": "alice"},
                    "attributes": {"tier": "gold"}
                },
                "onBehalfOf": {"kind": "service", "id": "crm"},
                "delegationChain": [{"kind": "human", "id": "alice"}],
                "invocationId": "inv-1"
            })
        );
        let back: ActingContext = serde_json::from_value(value).unwrap();
        assert_eq!(back, acting);

        let minimal = ActingContext::new(
            AgentPrincipal::new(AgentId::new("urn:adk:agent:a").unwrap()),
            "inv-2",
        );
        let back: ActingContext =
            serde_json::from_value(serde_json::to_value(&minimal).unwrap()).unwrap();
        assert_eq!(back, minimal);
    }
}
