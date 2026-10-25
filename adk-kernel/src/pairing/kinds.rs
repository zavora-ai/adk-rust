//! What a paired device is, and the routes a server opens to devices beside the kernel's
//! (spec 3.3 §7.2, §10 K2).

use std::fmt;
use std::sync::Arc;

use axum::http::Method;
use serde::{Deserialize, Serialize};

use super::DeviceScope;
use crate::error::{KernelError, Result};

/// What a paired device is (spec 3.3 §7.2, §10 K2): it picks the row of §7.2 a device's
/// answers take. Whoever issues the pairing code sets it; the claim never does. A device
/// paired without one is a remote terminal.
///
/// # Example
///
/// ```rust
/// use adk_kernel::pairing::DeviceKind;
///
/// assert_eq!(serde_json::to_value(DeviceKind::Phone).unwrap(), "phone");
/// assert_eq!("tray".parse::<DeviceKind>().unwrap(), DeviceKind::Tray);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum DeviceKind {
    /// The web home installed on a phone (spec 3.3 §5.4).
    Phone,
    /// A terminal on another machine (spec 3.3 §5.2).
    Terminal,
    /// The desktop tray (spec 3.3 §5.5).
    Tray,
}

impl DeviceKind {
    /// The kind's name: `phone`, `terminal` or `tray`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Phone => "phone",
            Self::Terminal => "terminal",
            Self::Tray => "tray",
        }
    }
}

impl fmt::Display for DeviceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for DeviceKind {
    type Err = KernelError;

    fn from_str(text: &str) -> Result<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "phone" => Ok(Self::Phone),
            "terminal" => Ok(Self::Terminal),
            "tray" => Ok(Self::Tray),
            other => Err(KernelError::Config {
                reason: format!("unknown device kind `{other}`: use phone, terminal or tray"),
            }),
        }
    }
}

/// The device scope a server's own routes need, consulted after the kernel's
/// [`device_scope_for`](super::device_scope_for): the server names its surface routes, so the
/// kernel never does. `None` closes a route to devices.
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
/// use axum::http::Method;
/// use adk_kernel::pairing::{DeviceRoutes, DeviceScope};
///
/// let routes: DeviceRoutes = Arc::new(|method: &Method, path: &str| {
///     (method == Method::GET && path.starts_with("/api/surface/")).then_some(DeviceScope::Read)
/// });
/// assert_eq!(routes(&Method::GET, "/api/surface/status"), Some(DeviceScope::Read));
/// ```
pub type DeviceRoutes = Arc<dyn Fn(&Method, &str) -> Option<DeviceScope> + Send + Sync>;
