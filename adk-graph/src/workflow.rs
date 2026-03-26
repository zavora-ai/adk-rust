//! Workflow schema types and loader for action node graph workflows.
//!
//! This module provides the `WorkflowSchema` type for serializing and deserializing
//! graph workflows that include action nodes, and a builder that constructs a
//! `GraphAgent` from a schema.

use std::collections::HashMap;
use std::sync::Arc;

use adk_action::{ActionNodeConfig, SwitchNodeConfig, TriggerNodeConfig};
use serde::{Deserialize, Serialize};

use crate::action::ActionNodeExecutor;
use crate::action::switch::evaluate_switch_conditions;
use crate::edge::{END, EdgeTarget};
use crate::error::{GraphError, Result};
use crate::graph::StateGraph;
use crate::node::Node;
use crate::state::StateSchema;

// ── WorkflowSchema types ──────────────────────────────────────────────

/// An edge in the workflow graph, matching adk-studio's edge format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEdge {
    /// Source node ID.
    pub from: String,
    /// Target node ID.
    pub to: String,
    /// Optional condition ID that must be satisfied for this edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// Optional source port identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_port: Option<String>,
    /// Optional target port identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_port: Option<String>,
}

/// A condition used by conditional edges in the workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowCondition {
    /// Unique condition identifier.
    pub id: String,
    /// The condition expression to evaluate.
    pub expression: String,
    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The serializable representation of a graph workflow including nodes, edges,
/// conditions, and action node configurations.
///
/// This is the interchange format between adk-studio (visual builder) and
/// adk-graph (runtime engine).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowSchema {
    /// Edges connecting nodes in the graph.
    pub edges: Vec<WorkflowEdge>,
    /// Conditions referenced by conditional edges.
    #[serde(default)]
    pub conditions: Vec<WorkflowCondition>,
    /// Action node configurations keyed by node ID.
    #[serde(default)]
    pub action_nodes: HashMap<String, ActionNodeConfig>,
    /// Agent node IDs (LLM agent nodes registered externally).
    #[serde(default)]
    pub agent_nodes: Vec<String>,
}

impl WorkflowSchema {
    /// Deserialize a `WorkflowSchema` from a JSON string.
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json)
            .map_err(|e| GraphError::InvalidGraph(format!("invalid workflow JSON: {e}")))
    }

    /// Build a `GraphAgent` from this workflow schema.
    ///
    /// 1. Creates a `StateGraph`
    /// 2. Registers each action node via `ActionNodeExecutor`
    /// 3. Registers edges (direct and conditional)
    /// 4. Handles Switch nodes by auto-registering conditional edges
    /// 5. Compiles and returns a `GraphAgent`
    pub fn build_graph(&self, name: &str) -> Result<crate::agent::GraphAgent> {
        let schema = StateSchema::simple(&["input", "output", "messages"]);
        let mut graph = StateGraph::new(schema);

        // Register action nodes
        for (node_id, config) in &self.action_nodes {
            let executor = ActionNodeExecutor::new(config.clone());
            // Verify the executor's name matches the expected node_id
            debug_assert_eq!(
                executor.name(),
                node_id,
                "ActionNodeExecutor name must match node ID"
            );
            graph.nodes.insert(node_id.clone(), Arc::new(executor));
        }

        // Build a condition lookup map
        let condition_map: HashMap<&str, &WorkflowCondition> =
            self.conditions.iter().map(|c| (c.id.as_str(), c)).collect();

        // Group edges by source to detect conditional routing
        let mut conditional_groups: HashMap<String, Vec<&WorkflowEdge>> = HashMap::new();
        let mut direct_edges: Vec<&WorkflowEdge> = Vec::new();

        for edge in &self.edges {
            if edge.condition.is_some() {
                conditional_groups.entry(edge.from.clone()).or_default().push(edge);
            } else {
                direct_edges.push(edge);
            }
        }

        // Register direct edges
        for edge in &direct_edges {
            graph = graph.add_edge(&edge.from, &edge.to);
        }

        // Register conditional edges from Switch nodes
        for (node_id, config) in &self.action_nodes {
            if let ActionNodeConfig::Switch(switch_config) = config {
                graph = register_switch_conditional_edges(graph, node_id, switch_config);
            }
        }

        // Register conditional edges from schema conditions
        for (source, edges) in &conditional_groups {
            // Skip if this source is a Switch node (already handled above)
            if let Some(ActionNodeConfig::Switch(_)) = self.action_nodes.get(source) {
                continue;
            }

            // Build targets map and router for condition-based edges
            let mut targets_map: HashMap<String, EdgeTarget> = HashMap::new();
            let mut condition_expressions: Vec<(String, String)> = Vec::new();

            for edge in edges {
                if let Some(cond_id) = &edge.condition {
                    let target = if edge.to == END {
                        EdgeTarget::End
                    } else {
                        EdgeTarget::Node(edge.to.clone())
                    };
                    targets_map.insert(edge.to.clone(), target);

                    if let Some(cond) = condition_map.get(cond_id.as_str()) {
                        condition_expressions.push((cond.expression.clone(), edge.to.clone()));
                    }
                }
            }

            let router_expressions = condition_expressions.clone();
            let default_target = END.to_string();

            let router = Arc::new(move |state: &crate::state::State| -> String {
                for (expr, target) in &router_expressions {
                    let resolved = adk_action::interpolate_variables(expr, state);
                    let trimmed = resolved.trim().to_lowercase();
                    if !trimmed.is_empty() && trimmed != "false" && trimmed != "0" {
                        return target.clone();
                    }
                }
                default_target.clone()
            });

            graph.edges.push(crate::edge::Edge::Conditional {
                source: source.clone(),
                router,
                targets: targets_map,
            });
        }

        let compiled = graph.compile()?;
        Ok(crate::agent::GraphAgent::from_graph(name, compiled))
    }

    /// Extract trigger node configurations from the action nodes.
    ///
    /// Returns all `TriggerNodeConfig` entries found in the workflow.
    pub fn trigger_configs(&self) -> Vec<TriggerNodeConfig> {
        self.action_nodes
            .values()
            .filter_map(|config| {
                if let ActionNodeConfig::Trigger(trigger) = config {
                    Some(trigger.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Build a `TriggerRuntime` from this workflow schema.
    ///
    /// Extracts trigger node configurations and constructs a `TriggerRuntime`
    /// with the provided graph agent.
    #[cfg(feature = "action-trigger")]
    pub fn build_trigger_runtime(
        &self,
        graph: Arc<crate::agent::GraphAgent>,
    ) -> crate::action::trigger_runtime::TriggerRuntime {
        let triggers = self.trigger_configs();
        crate::action::trigger_runtime::TriggerRuntime::new(graph, triggers)
    }
}

/// Register conditional edges for a Switch node.
///
/// Creates a conditional edge from the switch node to each condition's output port,
/// using the switch condition evaluation logic as the router.
fn register_switch_conditional_edges(
    mut graph: StateGraph,
    node_id: &str,
    switch_config: &SwitchNodeConfig,
) -> StateGraph {
    let conditions = switch_config.conditions.clone();
    let eval_mode = switch_config.evaluation_mode.clone();
    let default_branch = switch_config.default_branch.clone();

    // Build targets map from conditions
    let mut targets_map: HashMap<String, EdgeTarget> = HashMap::new();
    for condition in &conditions {
        targets_map
            .insert(condition.output_port.clone(), EdgeTarget::Node(condition.output_port.clone()));
    }
    if let Some(ref default) = default_branch {
        let target =
            if default == END { EdgeTarget::End } else { EdgeTarget::Node(default.clone()) };
        targets_map.insert(default.clone(), target);
    }
    // Always include END as a possible target
    targets_map.insert(END.to_string(), EdgeTarget::End);

    let router = Arc::new(move |state: &crate::state::State| -> String {
        match evaluate_switch_conditions(&conditions, state, &eval_mode, default_branch.as_deref())
        {
            Ok(ports) => ports.into_iter().next().unwrap_or_else(|| END.to_string()),
            Err(_) => END.to_string(),
        }
    });

    graph.edges.push(crate::edge::Edge::Conditional {
        source: node_id.to_string(),
        router,
        targets: targets_map,
    });

    graph
}
