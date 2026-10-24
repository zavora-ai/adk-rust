//! The managed form of the kernel (spec 3.2 §7.13, §10 M1; feature `managed`).
//!
//! The kernel hosts a user's or a team's agents on their own infrastructure; the managed
//! runtime (`adk-managed`) hosts tenants. They share the durable stores and the harness:
//!
//! | Item | What it is |
//! |---|---|
//! | [`KernelDef`] | Partitions and their coordinator, driver manifests, the proactivity policy and wake sets, as a block under the [`KERNEL_DEF_METADATA_KEY`] metadata key of a `ManagedAgentDef`, beside the harness's `adk.harness` |
//! | [`TenantKernel`] | One kernel over one tenant's `ManagedStores`: the tenant's definitions as targets, their wake sets as triggers, their proactivity policy, the tenant's approval service waking parked processes |
//! | [`KernelResolver`] | The `AgentResolver` the tenant's runtime is built with: it hosts each definition carrying a kernel block on the tenant's kernel |
//! | [`PartitionAgentResolver`], [`PartitionHost`] | One agent per partition a definition declares — one harness per partition — over the tenant's partition table (spec 3.2 §7.8) |
//! | [`ManagedKernels`] | A host's tenants, one kernel each, refusing a tenant whose stores another holds |
//! | [`DRIVER_MOUNT_TOOL_NAME`] | The inbox tool name of a driver mount a definition asks for: a person approves it before the tenant's supervisor mounts it (spec 3.2 §7.7) |
//!
//! A definition comes from a tenant, so its kernel block cannot widen what the operator
//! allows: its agent is built by the operator's resolver, a driver is never unsandboxed and
//! mounts only with a person's approval, a wake set never takes the queue a person's turn
//! holds, and the consolidation tick is off unless the block turns it on (§13 Q1).
//! `adk-managed` is Experimental, so this module is too.
//!
//! # Example
//!
//! ```rust
//! use adk_kernel::Wake;
//! use adk_kernel::managed::{KernelDef, KERNEL_DEF_METADATA_KEY, WakeSetDef};
//! use adk_managed::types::{ManagedAgentDef, ModelRef};
//!
//! let def = KernelDef::new()
//!     .with_wake_set(WakeSetDef::new("orders", [Wake::Webhook { hook: "orders".into() }], "Triage the new orders."))
//!     .apply_to(ManagedAgentDef::new("triage", ModelRef::Shorthand("claude-sonnet-5-5".into())));
//! let block = &def.metadata.as_ref().unwrap()[KERNEL_DEF_METADATA_KEY];
//! assert!(block.contains(r#""hook":"orders""#));
//! ```

mod def;
mod tenant;

pub use def::{
    CROSS_PARTITION_VALUES, DEFAULT_WAKE_USER, KERNEL_DEF_METADATA_KEY, KernelDef, PartitionDef,
    WakeOverlap, WakeSetDef,
};
pub use tenant::{
    DRIVER_MOUNT_TOOL_NAME, KernelResolver, ManagedKernels, PartitionAgentResolver, PartitionHost,
    TenantKernel, TenantKernelBuilder,
};
