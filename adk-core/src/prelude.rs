//! The ADK Core Prelude
//!
//! This module provides easy access to the most commonly used traits and types
//! required for building and interacting with ADK agents.

// 1. Traits
pub use crate::agent::Agent;
pub use crate::model::Llm;
pub use crate::tool::Tool;

// 2. Foundational Types
pub use crate::types::{AdkIdentity, Content, InvocationId, Part, Role, SessionId, UserId};

// 3. Error Handling
pub use crate::error::{AdkError, Result as AdkResult};

// 4. Core Context/Events
pub use crate::context::{AdkContext, InvocationContext};
pub use crate::event::Event;
