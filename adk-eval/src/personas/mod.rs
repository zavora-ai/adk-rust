//! User personas for agent evaluation.
//!
//! This module provides simulated user personas for generating realistic
//! multi-turn test conversations during agent evaluation.
//!
//! ## Components
//!
//! - [`PersonaProfile`] — Structured definition of a simulated user's behavior
//! - [`UserSimulator`] — Generates user messages according to a persona
//! - [`PersonaRegistry`] — Loads and manages persona definitions
//!
//! ## Feature Flag
//!
//! This module is gated behind the `personas` feature flag:
//!
//! ```toml
//! [dependencies]
//! adk-eval = { version = "...", features = ["personas"] }
//! ```

mod profile;
mod registry;
mod simulator;

pub use profile::{ExpertiseLevel, PersonaProfile, PersonaTraits, Verbosity};
pub use registry::PersonaRegistry;
pub use simulator::UserSimulator;
