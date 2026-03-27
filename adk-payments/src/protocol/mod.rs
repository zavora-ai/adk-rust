//! Protocol adapter entry points.
//!
//! The adapter layer tracks:
//! - ACP stable `2026-01-30`
//! - ACP experimental extensions behind `acp-experimental`
//! - AP2 `v0.1-alpha` as of `2026-03-22`

#[cfg(feature = "acp")]
#[cfg_attr(docsrs, doc(cfg(feature = "acp")))]
pub mod acp;

#[cfg(feature = "ap2")]
#[cfg_attr(docsrs, doc(cfg(feature = "ap2")))]
pub mod ap2;
