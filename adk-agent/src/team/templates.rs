use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use adk_core::Agent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{LoopAgent, ParallelAgent, SequentialAgent};

use super::{RelationshipKind, TeamError, TeamMemberSpec, TeamPolicy, TeamRelationship, TeamSpec};

/// Portable LLM-directed team architecture presets.
///
/// Presets lower to ordinary [`TeamSpec`] values; they are not new atomic
/// agent kinds and remain inspectable and editable after lowering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "architecture")]
pub enum TeamArchitectureTemplate {
    /// One coordinator invokes or transfers to a flat specialist roster.
    Supervisor {
        /// Team root name.
        name: String,
        /// Coordinator member name.
        coordinator: String,
        /// Specialist member names.
        specialists: Vec<String>,
        /// Relationship semantics for each specialist edge.
        relationship: RelationshipKind,
        /// Runtime policy.
        #[serde(default)]
        policy: TeamPolicy,
    },
    /// One router dispatches to exact route targets.
    Router {
        /// Team root name.
        name: String,
        /// Router member name.
        router: String,
        /// Exact route targets.
        routes: Vec<String>,
        /// Whether dispatch delegates and returns or hands off control.
        relationship: RelationshipKind,
        /// Runtime policy.
        #[serde(default)]
        policy: TeamPolicy,
    },
    /// Root manager delegates through named branch managers to their workers.
    Hierarchical {
        /// Team root name.
        name: String,
        /// Root manager member name.
        root: String,
        /// Manager branches and their exact workers.
        branches: Vec<TeamManagerBranch>,
        /// Runtime policy.
        #[serde(default)]
        policy: TeamPolicy,
    },
}

/// One manager branch in a hierarchical architecture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TeamManagerBranch {
    /// Manager member name.
    pub manager: String,
    /// Exact worker member names.
    pub workers: Vec<String>,
}

impl TeamArchitectureTemplate {
    /// Lowers this preset to a validated, fully explicit [`TeamSpec`].
    pub fn lower(&self) -> std::result::Result<TeamSpec, TeamError> {
        let spec = match self {
            Self::Supervisor { name, coordinator, specialists, relationship, policy } => TeamSpec {
                name: name.clone(),
                description: "Supervisor team compiled from a portable architecture template"
                    .to_string(),
                coordinator: coordinator.clone(),
                members: std::iter::once(coordinator)
                    .chain(specialists)
                    .map(|member| TeamMemberSpec::new(member.clone()))
                    .collect(),
                relationships: specialists
                    .iter()
                    .map(|specialist| TeamRelationship::new(coordinator, specialist, *relationship))
                    .collect(),
                policy: policy.clone(),
            },
            Self::Router { name, router, routes, relationship, policy } => TeamSpec {
                name: name.clone(),
                description: "Exact router team compiled from a portable architecture template"
                    .to_string(),
                coordinator: router.clone(),
                members: std::iter::once(router)
                    .chain(routes)
                    .map(|member| TeamMemberSpec::new(member.clone()))
                    .collect(),
                relationships: routes
                    .iter()
                    .map(|route| TeamRelationship::new(router, route, *relationship))
                    .collect(),
                policy: policy.clone(),
            },
            Self::Hierarchical { name, root, branches, policy } => {
                let mut members = vec![TeamMemberSpec::new(root.clone())];
                let mut relationships = Vec::new();
                for branch in branches {
                    members.push(TeamMemberSpec::new(branch.manager.clone()));
                    relationships.push(TeamRelationship::new(
                        root,
                        &branch.manager,
                        RelationshipKind::Delegate,
                    ));
                    for worker in &branch.workers {
                        members.push(TeamMemberSpec::new(worker.clone()));
                        relationships.push(TeamRelationship::new(
                            &branch.manager,
                            worker,
                            RelationshipKind::Delegate,
                        ));
                    }
                }
                TeamSpec {
                    name: name.clone(),
                    description: "Hierarchical team compiled from a portable architecture template"
                        .to_string(),
                    coordinator: root.clone(),
                    members,
                    relationships,
                    policy: policy.clone(),
                }
            }
        };
        spec.validate()?;
        Ok(spec)
    }
}

/// Portable deterministic workflow presets built from existing agent types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", tag = "architecture")]
pub enum WorkflowArchitectureTemplate {
    /// Execute members once in order.
    Sequential {
        /// Workflow root name.
        name: String,
        /// Ordered member names.
        steps: Vec<String>,
    },
    /// Execute members concurrently.
    Parallel {
        /// Workflow root name.
        name: String,
        /// Concurrent member names.
        members: Vec<String>,
        /// Share a concurrency-safe blackboard between members.
        #[serde(default)]
        shared_state: bool,
    },
    /// Fan out to workers in parallel, then run an aggregator.
    FanOutFanIn {
        /// Workflow root name.
        name: String,
        /// Parallel worker names.
        workers: Vec<String>,
        /// Aggregator member name.
        aggregator: String,
        /// Share state between fan-out workers.
        #[serde(default)]
        shared_state: bool,
    },
    /// Alternate producer and reviewer for a bounded number of iterations.
    ReviewLoop {
        /// Workflow root name.
        name: String,
        /// Producer member name.
        producer: String,
        /// Reviewer member name.
        reviewer: String,
        /// Hard iteration bound.
        max_iterations: u32,
    },
}

impl WorkflowArchitectureTemplate {
    /// Binds member names and compiles to existing deterministic agent primitives.
    pub fn compile(
        &self,
        agents: impl IntoIterator<Item = Arc<dyn Agent>>,
    ) -> std::result::Result<Arc<dyn Agent>, TeamError> {
        let registry: HashMap<String, Arc<dyn Agent>> =
            agents.into_iter().map(|agent| (agent.name().to_string(), agent)).collect();
        let resolve = |names: &[String]| -> std::result::Result<Vec<Arc<dyn Agent>>, TeamError> {
            names
                .iter()
                .map(|name| {
                    registry.get(name).cloned().ok_or_else(|| TeamError::MissingAgent(name.clone()))
                })
                .collect()
        };
        let ensure_unique = |names: &[String]| -> std::result::Result<(), TeamError> {
            let mut seen = HashSet::new();
            for name in names {
                if !seen.insert(name) {
                    return Err(TeamError::DuplicateMember(name.clone()));
                }
            }
            Ok(())
        };
        match self {
            Self::Sequential { name, steps } => {
                ensure_unique(steps)?;
                Ok(Arc::new(SequentialAgent::new(name, resolve(steps)?)))
            }
            Self::Parallel { name, members, shared_state } => {
                ensure_unique(members)?;
                let parallel = ParallelAgent::new(name, resolve(members)?);
                Ok(Arc::new(if *shared_state { parallel.with_shared_state() } else { parallel }))
            }
            Self::FanOutFanIn { name, workers, aggregator, shared_state } => {
                ensure_unique(workers)?;
                if workers.iter().any(|worker| worker == aggregator) {
                    return Err(TeamError::DuplicateMember(aggregator.clone()));
                }
                let parallel = ParallelAgent::new(format!("{name}_fan_out"), resolve(workers)?);
                let parallel = if *shared_state { parallel.with_shared_state() } else { parallel };
                let aggregator = registry
                    .get(aggregator)
                    .cloned()
                    .ok_or_else(|| TeamError::MissingAgent(aggregator.clone()))?;
                Ok(Arc::new(SequentialAgent::new(name, vec![Arc::new(parallel), aggregator])))
            }
            Self::ReviewLoop { name, producer, reviewer, max_iterations } => {
                if *max_iterations == 0 {
                    return Err(TeamError::InvalidPolicy("reviewLoop.maxIterations"));
                }
                if producer == reviewer {
                    return Err(TeamError::DuplicateMember(producer.clone()));
                }
                let members = resolve(&[producer.clone(), reviewer.clone()])?;
                Ok(Arc::new(LoopAgent::new(name, members).with_max_iterations(*max_iterations)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopAgent(String);

    #[async_trait::async_trait]
    impl Agent for NoopAgent {
        fn name(&self) -> &str {
            &self.0
        }

        fn description(&self) -> &str {
            "workflow template test agent"
        }

        fn sub_agents(&self) -> &[Arc<dyn Agent>] {
            &[]
        }

        async fn run(
            &self,
            _ctx: Arc<dyn adk_core::InvocationContext>,
        ) -> adk_core::Result<adk_core::EventStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[test]
    fn supervisor_template_lowers_to_exact_edges() {
        let spec = TeamArchitectureTemplate::Supervisor {
            name: "support".to_string(),
            coordinator: "supervisor".to_string(),
            specialists: vec!["billing".to_string(), "technical".to_string()],
            relationship: RelationshipKind::Handoff,
            policy: TeamPolicy::default(),
        }
        .lower()
        .unwrap();
        assert_eq!(spec.members.len(), 3);
        assert_eq!(spec.relationships.len(), 2);
        assert!(spec.relationships.iter().all(|edge| edge.from == "supervisor"));
    }

    #[test]
    fn hierarchical_template_rejects_duplicate_workers() {
        let error = TeamArchitectureTemplate::Hierarchical {
            name: "org".to_string(),
            root: "director".to_string(),
            branches: vec![
                TeamManagerBranch {
                    manager: "research".to_string(),
                    workers: vec!["analyst".to_string()],
                },
                TeamManagerBranch {
                    manager: "review".to_string(),
                    workers: vec!["analyst".to_string()],
                },
            ],
            policy: TeamPolicy::default(),
        }
        .lower()
        .unwrap_err();
        assert_eq!(error, TeamError::DuplicateMember("analyst".to_string()));
    }

    #[test]
    fn workflow_template_compiles_to_existing_primitives() {
        let root = WorkflowArchitectureTemplate::FanOutFanIn {
            name: "research".to_string(),
            workers: vec!["facts".to_string(), "risks".to_string()],
            aggregator: "reviewer".to_string(),
            shared_state: true,
        }
        .compile([
            Arc::new(NoopAgent("facts".to_string())) as Arc<dyn Agent>,
            Arc::new(NoopAgent("risks".to_string())),
            Arc::new(NoopAgent("reviewer".to_string())),
        ])
        .unwrap();
        assert_eq!(root.name(), "research");
        assert_eq!(root.sub_agents().len(), 2);
        assert_eq!(root.sub_agents()[0].name(), "research_fan_out");
    }
}
