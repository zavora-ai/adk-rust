//! Wire types for the Enterprise Managed Agent Service.
//!
//! These types are self-contained copies of the service's wire format, with no dependency
//! on the `adk-*` runtime crates.
//! They serialize/deserialize using Rust's native snake_case field names,
//! matching the server's wire format. Enum variants that require case
//! conversion retain `#[serde(rename_all = "camelCase")]` (e.g., PermissionMode).

pub mod agent;
pub mod environment;
pub mod events;
pub mod memory;
pub mod model_ref;
pub mod pagination;
pub mod session;
pub mod tools;
pub mod vault;

pub use agent::*;
pub use environment::*;
pub use events::*;
pub use memory::*;
pub use model_ref::*;
pub use pagination::*;
pub use session::*;
pub use tools::*;
pub use vault::*;
