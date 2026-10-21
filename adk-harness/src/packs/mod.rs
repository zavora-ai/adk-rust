//! Packs: features that add tools or a surface under the harness policy
//! (spec 3.1 §6 D-8, §7.16–§7.18).
//!
//! | Pack | Feature | Spec |
//! |---|---|---|
//! | `payments` | `payments` | §7.16 |
//! | `managed` | `managed` | §7.18 |

#[cfg(feature = "managed")]
pub mod managed;
#[cfg(feature = "payments")]
pub mod payments;
