//! Human-in-the-loop interrupt types

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Interrupt request from a node or configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Interrupt {
    /// Interrupt before executing a node
    Before(String),
    /// Interrupt after executing a node
    After(String),
    /// Dynamic interrupt from within a node
    Dynamic {
        /// Message to display to the user
        message: String,
        /// Optional data for the interrupt
        data: Option<Value>,
    },
}

impl std::fmt::Display for Interrupt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Before(node) => write!(f, "Interrupt before '{}'", node),
            Self::After(node) => write!(f, "Interrupt after '{}'", node),
            Self::Dynamic { message, .. } => write!(f, "Dynamic interrupt: {}", message),
        }
    }
}

/// Helper to create a dynamic interrupt from within a node
pub fn interrupt(message: &str) -> Interrupt {
    Interrupt::Dynamic { message: message.to_string(), data: None }
}

/// Helper to create a dynamic interrupt with data
pub fn interrupt_with_data(message: &str, data: Value) -> Interrupt {
    Interrupt::Dynamic { message: message.to_string(), data: Some(data) }
}
