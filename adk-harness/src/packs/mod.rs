//! Packs: features that add tools or a surface under the harness policy
//! (spec 3.1 §6 D-8, §7.16–§7.18, §7.24).
//!
//! | Pack | Feature | Spec |
//! |---|---|---|
//! | `browser` | `browser` | §7.24 |
//! | `computer_use` | `computer-use` | §7.24 |
//! | `payments` | `payments` | §7.16 |
//! | `managed` | `managed` | §7.18 |

#[cfg(feature = "browser")]
pub mod browser;
#[cfg(feature = "computer-use")]
pub mod computer_use;
#[cfg(feature = "managed")]
pub mod managed;
#[cfg(feature = "payments")]
pub mod payments;
