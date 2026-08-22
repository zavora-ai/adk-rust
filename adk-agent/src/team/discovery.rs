use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::Arc;

use adk_core::Agent;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{CompiledTeam, ResolvedTeamMember, TeamError, TeamMemberSpec, TeamSpec};

/// Health advertised by a registry candidate at resolution time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub enum TeamAgentHealth {
    /// Candidate is accepting work normally.
    #[default]
    Healthy,
    /// Candidate is available with reduced capacity or functionality.
    Degraded,
    /// Candidate must not receive new work.
    Unavailable,
}

/// Serializable candidate metadata returned by a team agent registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamAgentDescriptor {
    /// Registry-specific immutable binding identifier.
    pub binding: String,
    /// Agent's advertised name.
    pub name: String,
    /// Human-readable capability summary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Machine-comparable capability identifiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Higher-priority candidates win deterministic selection.
    #[serde(default)]
    pub priority: i32,
    /// Provider or semantic version of this immutable binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Immutable content or configuration digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Deployment-defined trust labels such as `internal` or `pii-approved`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trust_labels: Vec<String>,
    /// Current registry health.
    #[serde(default)]
    pub health: TeamAgentHealth,
    /// Unix expiry timestamp in milliseconds for ephemeral registrations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
}

/// Discovers and resolves local or remote agents for portable team members.
#[async_trait]
pub trait TeamAgentRegistry: Send + Sync {
    /// Lists candidates visible to the current deployment and caller.
    async fn candidates(&self) -> adk_core::Result<Vec<TeamAgentDescriptor>>;

    /// Resolves one immutable binding identifier to an executable agent.
    async fn resolve(&self, binding: &str) -> adk_core::Result<Arc<dyn Agent>>;

    /// Authorizes a candidate for one portable member after metadata filtering.
    async fn authorize(
        &self,
        _member: &TeamMemberSpec,
        _candidate: &TeamAgentDescriptor,
    ) -> adk_core::Result<bool> {
        Ok(true)
    }
}

/// Deterministic in-process registry useful for generated projects and tests.
#[derive(Default)]
pub struct StaticTeamAgentRegistry {
    entries: HashMap<String, (TeamAgentDescriptor, Arc<dyn Agent>)>,
}

impl StaticTeamAgentRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one immutable descriptor and executable binding.
    pub fn register(
        mut self,
        descriptor: TeamAgentDescriptor,
        agent: Arc<dyn Agent>,
    ) -> std::result::Result<Self, TeamError> {
        if self.entries.contains_key(&descriptor.binding) {
            return Err(TeamError::Registry(format!(
                "duplicate team registry binding '{}'",
                descriptor.binding
            )));
        }
        self.entries.insert(descriptor.binding.clone(), (descriptor, agent));
        Ok(self)
    }
}

#[async_trait]
impl TeamAgentRegistry for StaticTeamAgentRegistry {
    async fn candidates(&self) -> adk_core::Result<Vec<TeamAgentDescriptor>> {
        Ok(self.entries.values().map(|(descriptor, _)| descriptor.clone()).collect())
    }

    async fn resolve(&self, binding: &str) -> adk_core::Result<Arc<dyn Agent>> {
        self.entries
            .get(binding)
            .map(|(_, agent)| agent.clone())
            .ok_or_else(|| adk_core::AdkError::agent(format!("unknown team binding '{binding}'")))
    }
}

impl TeamSpec {
    /// Resolves missing concrete members by capability and freezes the roster.
    ///
    /// Selection is reproducible: exact advertised-name matches win, followed
    /// by descending registry priority and then lexical binding identifier.
    pub async fn compile_with_registry(
        &self,
        registry: Arc<dyn TeamAgentRegistry>,
    ) -> std::result::Result<CompiledTeam, TeamError> {
        self.validate()?;
        #[cfg(not(feature = "team-tools"))]
        if self.relationships.iter().any(|edge| edge.kind == super::RelationshipKind::Delegate) {
            return Err(TeamError::DelegationFeatureDisabled);
        }
        let candidates =
            registry.candidates().await.map_err(|error| TeamError::Registry(error.to_string()))?;
        let mut bindings = HashMap::new();
        let mut frozen = Vec::with_capacity(self.members.len());
        for member in &self.members {
            let mut eligible: Vec<&TeamAgentDescriptor> = candidates
                .iter()
                .filter(|candidate| {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let has_capabilities = member.required_capabilities.iter().all(|required| {
                        candidate.capabilities.iter().any(|available| available == required)
                    });
                    let health_allowed = candidate.health != TeamAgentHealth::Unavailable
                        && (!member.registry.require_healthy
                            || candidate.health == TeamAgentHealth::Healthy);
                    let trust_allowed = member.registry.trust_labels.is_empty()
                        || member.registry.trust_labels.iter().any(|required| {
                            candidate.trust_labels.iter().any(|label| label == required)
                        });
                    has_capabilities
                        && health_allowed
                        && trust_allowed
                        && candidate.expires_at_ms.is_none_or(|expiry| expiry > now_ms)
                        && member
                            .registry
                            .version
                            .as_ref()
                            .is_none_or(|version| candidate.version.as_ref() == Some(version))
                        && member
                            .registry
                            .digest
                            .as_ref()
                            .is_none_or(|digest| candidate.digest.as_ref() == Some(digest))
                        && (!member.required_capabilities.is_empty()
                            || candidate.name == member.name)
                })
                .collect();
            eligible.sort_by_key(|candidate| {
                (
                    Reverse(candidate.name == member.name),
                    Reverse(candidate.priority),
                    candidate.binding.as_str(),
                )
            });
            let mut selected = None;
            for candidate in eligible {
                if registry
                    .authorize(member, candidate)
                    .await
                    .map_err(|error| TeamError::Registry(error.to_string()))?
                {
                    selected = Some(candidate);
                    break;
                }
            }
            let selected = selected.ok_or_else(|| TeamError::NoRegistryCandidate {
                member: member.name.clone(),
                capabilities: member.required_capabilities.clone(),
            })?;
            let agent = registry
                .resolve(&selected.binding)
                .await
                .map_err(|error| TeamError::Registry(error.to_string()))?;
            bindings.insert(member.name.clone(), agent);
            frozen.push(ResolvedTeamMember {
                member: member.name.clone(),
                binding: selected.binding.clone(),
                capabilities: selected.capabilities.clone(),
                version: selected.version.clone(),
                digest: selected.digest.clone(),
                trust_labels: selected.trust_labels.clone(),
            });
        }
        self.compile_registry(bindings, frozen, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TeamMemberSpec;
    use adk_core::{EventStream, InvocationContext, Result};

    struct NamedAgent(String);

    #[async_trait]
    impl Agent for NamedAgent {
        fn name(&self) -> &str {
            &self.0
        }

        fn description(&self) -> &str {
            "registry test agent"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(&self, _ctx: Arc<dyn InvocationContext>) -> Result<EventStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    fn descriptor(
        binding: &str,
        name: &str,
        capabilities: &[&str],
        priority: i32,
    ) -> TeamAgentDescriptor {
        TeamAgentDescriptor {
            binding: binding.to_string(),
            name: name.to_string(),
            description: String::new(),
            capabilities: capabilities.iter().map(ToString::to_string).collect(),
            priority,
            version: None,
            digest: None,
            trust_labels: Vec::new(),
            health: TeamAgentHealth::Healthy,
            expires_at_ms: None,
        }
    }

    #[tokio::test]
    async fn resolves_capabilities_deterministically_and_freezes_roster() {
        let spec = TeamSpec {
            name: "discovered".to_string(),
            description: String::new(),
            coordinator: "planner".to_string(),
            members: vec![TeamMemberSpec::new("planner").with_capabilities(["plan"])],
            relationships: Vec::new(),
            policy: super::super::TeamPolicy::default(),
        };
        let registry = StaticTeamAgentRegistry::new()
            .register(
                descriptor("remote-low", "planner", &["plan"], 1),
                Arc::new(NamedAgent("remote_low".to_string())),
            )
            .unwrap()
            .register(
                descriptor("remote-high", "alternate", &["plan", "web"], 9),
                Arc::new(NamedAgent("remote_high".to_string())),
            )
            .unwrap();

        let team = spec.compile_with_registry(Arc::new(registry)).await.unwrap();
        team.runtime.check_budget("registry-test").unwrap();
        let snapshot = team.runtime.snapshot("registry-test").unwrap();
        assert_eq!(snapshot.roster[0].member, "planner");
        assert_eq!(snapshot.roster[0].binding, "remote-low");
        assert_eq!(team.sub_agents()[0].name(), "planner");
    }

    #[tokio::test]
    async fn rejects_registry_without_required_capabilities() {
        let spec = TeamSpec {
            name: "discovered".to_string(),
            description: String::new(),
            coordinator: "planner".to_string(),
            members: vec![TeamMemberSpec::new("planner").with_capabilities(["plan", "secure"])],
            relationships: Vec::new(),
            policy: super::super::TeamPolicy::default(),
        };
        let registry = StaticTeamAgentRegistry::new()
            .register(
                descriptor("candidate", "planner", &["plan"], 1),
                Arc::new(NamedAgent("candidate".to_string())),
            )
            .unwrap();
        assert!(matches!(
            spec.compile_with_registry(Arc::new(registry)).await,
            Err(TeamError::NoRegistryCandidate { .. })
        ));
    }

    #[tokio::test]
    async fn enforces_registry_health_trust_version_and_digest() {
        let requirement = super::super::TeamRegistryRequirement {
            version: Some("2.1.0".to_string()),
            digest: Some("sha256:trusted".to_string()),
            trust_labels: vec!["internal".to_string()],
            require_healthy: true,
        };
        let spec = TeamSpec {
            name: "governed".to_string(),
            description: String::new(),
            coordinator: "planner".to_string(),
            members: vec![TeamMemberSpec::new("planner").with_registry_requirement(requirement)],
            relationships: Vec::new(),
            policy: super::super::TeamPolicy::default(),
        };
        let mut rejected = descriptor("rejected", "planner", &[], 10);
        rejected.health = TeamAgentHealth::Degraded;
        rejected.version = Some("2.1.0".to_string());
        rejected.digest = Some("sha256:trusted".to_string());
        rejected.trust_labels.push("internal".to_string());
        let mut selected = descriptor("selected", "planner", &[], 1);
        selected.version = Some("2.1.0".to_string());
        selected.digest = Some("sha256:trusted".to_string());
        selected.trust_labels.push("internal".to_string());
        let registry = StaticTeamAgentRegistry::new()
            .register(rejected, Arc::new(NamedAgent("rejected".to_string())))
            .unwrap()
            .register(selected, Arc::new(NamedAgent("selected".to_string())))
            .unwrap();
        let team = spec.compile_with_registry(Arc::new(registry)).await.unwrap();
        team.runtime.check_budget("governed-run").unwrap();
        let roster = team.runtime.snapshot("governed-run").unwrap().roster;
        assert_eq!(roster[0].binding, "selected");
        assert_eq!(roster[0].version.as_deref(), Some("2.1.0"));
        assert_eq!(roster[0].trust_labels, ["internal"]);
    }
}
