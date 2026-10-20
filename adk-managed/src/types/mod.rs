//! Wire types for the managed agent runtime.
//!
//! The types serialize to the JSON a managed agent service sends and accepts: snake_case
//! fields, and tagged unions discriminated by a `type` field. Polymorphic types are
//! `#[non_exhaustive]` to allow additive evolution.

pub mod agent_def;
pub mod content;
pub mod error;
pub mod events;
pub mod model_ref;
pub mod session;
pub mod tools;

pub use agent_def::*;
pub use content::*;
pub use error::*;
pub use events::{ConfirmationResult, SessionEvent, StopReason, UserEvent};
pub use model_ref::*;
pub use session::*;
pub use tools::*;
