//! The identity ADK-Rust derives when none is configured, delegation, and the
//! conversion of [`PrincipalError`] into [`AdkError`].

use crate::principal::{
    ActingContext, AgentId, AgentPrincipal, MAX_AGENT_ID_LEN, PrincipalError, SubjectRef,
};
use crate::{AdkError, ErrorCategory, ErrorComponent};

impl From<PrincipalError> for AdkError {
    fn from(error: PrincipalError) -> Self {
        AdkError::new(
            ErrorComponent::Auth,
            ErrorCategory::InvalidInput,
            "identity.agent_id.invalid",
            error.to_string(),
        )
    }
}

impl AgentPrincipal {
    /// The principal ADK-Rust derives when none is configured:
    /// `urn:adk:agent:{app_name}/{agent_name}`.
    ///
    /// Characters outside `[A-Za-z0-9-._~]` are percent-encoded, so every app and agent
    /// name yields a valid [`AgentId`], and the same names always yield the same id.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::AgentPrincipal;
    ///
    /// let principal = AgentPrincipal::derived("billing app", "invoicer");
    /// assert_eq!(principal.id.as_str(), "urn:adk:agent:billing%20app/invoicer");
    /// ```
    pub fn derived(app_name: &str, agent_name: &str) -> Self {
        let encode = |value: &str| -> String {
            if value.is_empty() {
                return "_".to_string();
            }
            let mut out = String::with_capacity(value.len());
            for byte in value.bytes() {
                if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                    out.push(byte as char);
                } else {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
            out
        };
        let mut id = format!("urn:adk:agent:{}/{}", encode(app_name), encode(agent_name));
        // Percent-encoding at most triples a name, so only absurd names reach the cap;
        // the encoded id is ASCII, so truncation stays on a character boundary.
        id.truncate(MAX_AGENT_ID_LEN);
        Self::new(AgentId::new(id).expect("a percent-encoded URN is a valid agent id"))
    }
}

impl ActingContext {
    /// The acting context ADK-Rust derives when none is configured.
    ///
    /// The principal is [`AgentPrincipal::derived`] from `app_name` and `agent_name`,
    /// and the agent acts for `user_id` as a human subject when it is non-empty.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{ActingContext, SubjectRef};
    ///
    /// let acting = ActingContext::derived("shop", "cashier", "alice", "inv-1");
    /// assert_eq!(acting.agent.id.as_str(), "urn:adk:agent:shop/cashier");
    /// assert_eq!(acting.on_behalf_of, Some(SubjectRef::human("alice")));
    /// ```
    pub fn derived(app_name: &str, agent_name: &str, user_id: &str, invocation_id: &str) -> Self {
        let acting = Self::new(AgentPrincipal::derived(app_name, agent_name), invocation_id);
        if user_id.is_empty() { acting } else { acting.on_behalf_of(SubjectRef::human(user_id)) }
    }

    /// The context `delegate` acts in when this agent hands it a task.
    ///
    /// The delegate acts for the same subject, in `invocation_id`, and the chain gains
    /// this agent.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_core::{ActingContext, AgentPrincipal, SubjectRef};
    ///
    /// let planner = ActingContext::derived("ops", "planner", "alice", "inv-1");
    /// let researcher = planner.delegate(AgentPrincipal::derived("ops", "researcher"), "inv-2");
    /// assert_eq!(researcher.delegation_chain, vec![SubjectRef::agent(&planner.agent.id)]);
    /// assert_eq!(researcher.on_behalf_of, planner.on_behalf_of);
    /// ```
    #[must_use]
    pub fn delegate(&self, delegate: AgentPrincipal, invocation_id: impl Into<String>) -> Self {
        let mut delegated = Self::new(delegate, invocation_id);
        delegated.on_behalf_of = self.on_behalf_of.clone();
        delegated.delegation_chain = self.delegation_chain.clone();
        delegated.delegated_by(SubjectRef::agent(&self.agent.id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_ids_are_stable_and_always_valid() {
        assert_eq!(AgentPrincipal::derived("a", "b"), AgentPrincipal::derived("a", "b"));
        assert_eq!(AgentPrincipal::derived("", "").id.as_str(), "urn:adk:agent:_/_");
        assert_eq!(AgentPrincipal::derived("a/b", "c:d").id.as_str(), "urn:adk:agent:a%2Fb/c%3Ad");
        let long = AgentPrincipal::derived(&"\n".repeat(400), "agent");
        assert!(AgentId::new(long.id.as_str()).is_ok());
    }

    #[test]
    fn delegation_extends_the_chain_and_keeps_the_subject() {
        let acting = ActingContext::derived("app", "agent", "alice", "inv-1")
            .delegate(AgentPrincipal::derived("app", "helper").with_tenant("acme"), "inv-2");
        let json = serde_json::to_value(&acting).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "agent": { "id": "urn:adk:agent:app/helper", "tenant": "acme" },
                "onBehalfOf": { "kind": "human", "id": "alice" },
                "delegationChain": [{ "kind": "agent", "id": "urn:adk:agent:app/agent" }],
                "invocationId": "inv-2",
            })
        );
    }

    #[test]
    fn principal_errors_convert_with_a_structured_code() {
        let error: AdkError = AgentId::new("").unwrap_err().into();
        assert_eq!(error.code, "identity.agent_id.invalid");
    }
}
