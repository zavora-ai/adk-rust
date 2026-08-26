//! Hosted Google Cloud registry integrations for agent tooling.
//!
//! > **Note:** this module is gated behind the crate's
//! > `vertex-agent-registry` feature. It is **unrelated** to the umbrella
//! > crate's `agent-registry` feature, which enables the local YAML agent
//! > registry REST API in `adk-server`. This module talks to the hosted
//! > Google **Agent Registry** service at `agentregistry.googleapis.com`.

pub mod agent_registry;
