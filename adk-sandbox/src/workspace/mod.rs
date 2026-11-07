//! Workspace lifecycle layer for sandbox-agent harness.
//!
//! This module provides the persistent workspace lifecycle:
//! `provision → session → exec → snapshot → resume`.
//!
//! It is layered on top of the existing `SandboxBackend` trait without
//! modifying it, and is gated behind the `workspace` feature flag.
//!
//! # Feature Flags
//!
//! | Feature           | Description                              |
//! |-------------------|------------------------------------------|
//! | `workspace`       | Lifecycle types + LocalUnixClient        |
//! | `workspace-docker`| DockerClient (implies `workspace`)       |

// Sub-modules will be added by subsequent tasks:
pub mod client; // Task 1.11: SandboxClient trait
pub mod config; // Task 1.5: SandboxConfig, SandboxConfigSpec, Capability
#[cfg(feature = "workspace-docker")]
pub mod docker;
pub mod local_unix; // Task 3.1: LocalUnixClient
pub mod manifest; // Task 1.3: Manifest, ManifestEntry
pub mod path_safety; // Task 1.7: validate_relative_path
pub mod session; // Task 1.12: SandboxSession trait
pub mod types; // Task 1.2: SessionHandle, SnapshotId, ExecOutput, DirEntry, EntryType // Task 11.1: DockerClient

// Re-exports
pub use client::SandboxClient;
pub use config::{Capability, SandboxConfig, SandboxConfigSpec};
pub use local_unix::{LocalUnixClient, LocalUnixSession};
pub use manifest::{Manifest, ManifestEntry};
pub use path_safety::validate_relative_path;
pub use session::SandboxSession;
pub use types::{DirEntry, EntryType, ExecOutput, SessionHandle, SnapshotId};

#[cfg(feature = "workspace-docker")]
pub use docker::{DockerClient, DockerSession, ReadOnlyRoot};
