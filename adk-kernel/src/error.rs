//! The kernel's error type (spec 3.2 §7.1).
//!
//! Every fallible kernel API returns [`KernelError`]. It converts into
//! [`adk_core::AdkError`] at the boundary with an `ErrorComponent::Server` component and a
//! `kernel.*` code, so a control-plane route answers with the status and problem JSON the
//! rest of `adk-server` uses.

use adk_core::{AdkError, ErrorCategory, ErrorComponent, RunState};
use thiserror::Error;

use crate::kernel::KernelState;

/// Result of a fallible kernel operation (spec 3.2 §7.1).
pub type Result<T, E = KernelError> = std::result::Result<T, E>;

/// Error code of [`KernelError::NotFound`].
pub const NOT_FOUND_CODE: &str = "kernel.not_found";
/// Error code of [`KernelError::InvalidState`].
pub const INVALID_STATE_CODE: &str = "kernel.invalid_state";
/// Error code of [`KernelError::Frozen`].
pub const FROZEN_CODE: &str = "kernel.frozen";
/// Error code of [`KernelError::Lifecycle`].
pub const LIFECYCLE_CODE: &str = "kernel.lifecycle";
/// Error code of [`KernelError::Config`].
pub const CONFIG_CODE: &str = "kernel.config";
/// Error code of [`KernelError::Store`].
pub const STORE_CODE: &str = "kernel.store";
/// Error code of [`KernelError::ControlPlane`].
pub const CONTROL_PLANE_CODE: &str = "kernel.control_plane";
/// Error code of [`KernelError::Unauthorized`].
pub const UNAUTHORIZED_CODE: &str = "kernel.unauthorized";
/// Error code of [`KernelError::AgendaNotFound`].
pub const AGENDA_NOT_FOUND_CODE: &str = "kernel.agenda_not_found";
/// Error code of [`KernelError::AgendaState`].
pub const AGENDA_STATE_CODE: &str = "kernel.agenda_state";
/// Error code of [`KernelError::Quota`].
pub const QUOTA_CODE: &str = "kernel.quota";
/// Error code of [`KernelError::Io`].
pub const IO_CODE: &str = "kernel.io";
/// Error code of [`KernelError::Serde`].
pub const SERDE_CODE: &str = "kernel.serde";

/// What went wrong in the kernel (spec 3.2 §7.1).
///
/// Messages name the remedy: the command or setting that fixes the condition.
///
/// # Example
///
/// ```rust
/// use adk_core::AdkError;
/// use adk_kernel::KernelError;
///
/// let error = KernelError::NotFound { pid: "run-7".into() };
/// assert!(error.to_string().contains("adk-rust ps"));
/// let error: AdkError = error.into();
/// assert_eq!(error.code, adk_kernel::error::NOT_FOUND_CODE);
/// assert!(error.is_not_found());
/// ```
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum KernelError {
    /// No process has this id.
    #[error(
        "no process {pid}: list the processes with `adk-rust ps` or `GET /api/kernel/processes`"
    )]
    NotFound {
        /// The process id (the run id).
        pid: String,
    },

    /// The process is in a state the action does not apply to.
    #[error(
        "process {pid} is {state:?}, so it cannot be {action}; check its state with \
         `adk-rust ps`"
    )]
    InvalidState {
        /// The process id (the run id).
        pid: String,
        /// The run's state.
        state: RunState,
        /// The action refused, such as `suspended`.
        action: &'static str,
    },

    /// The kernel is frozen, so nothing starts.
    #[error("the kernel is frozen ({reason}); lift the freeze with `adk-rust unfreeze`")]
    Frozen {
        /// The freeze reason.
        reason: String,
    },

    /// The kernel's lifecycle state does not allow the call, such as `pause` before `start`.
    #[error("the kernel is {state}, so it cannot {action}; check `adk-rust ps` or the health")]
    Lifecycle {
        /// The kernel's state.
        state: KernelState,
        /// The refused call, such as `pause`.
        action: &'static str,
    },

    /// The kernel configuration is invalid.
    #[error("invalid kernel configuration: {reason}")]
    Config {
        /// What is wrong and how to fix it.
        reason: String,
    },

    /// A kernel store backend failed.
    #[error(
        "the {backend} kernel store failed: {reason}; check the store URL and that the server is reachable"
    )]
    Store {
        /// The backend, such as `sqlite`, `postgres` or `redis`.
        backend: &'static str,
        /// The backend's error.
        reason: String,
    },

    /// The control plane could not start, serve or connect.
    #[error("control plane: {reason}")]
    ControlPlane {
        /// What failed and how to fix it.
        reason: String,
    },

    /// A control-plane caller carries no authenticated subject (spec 3.2 §9.2 K-I16).
    #[error("unauthorized: {reason}")]
    Unauthorized {
        /// Why the caller was refused.
        reason: String,
    },

    /// No agenda item has this id (spec 3.2 §7.5).
    #[error(
        "no agenda item {item}: list the agenda with `adk-rust agenda` or `GET /api/kernel/agenda`"
    )]
    AgendaNotFound {
        /// The item id.
        item: String,
    },

    /// The agenda item is in a state the action does not apply to (spec 3.2 §7.5).
    #[error(
        "agenda item {item} is {state}, so it cannot be {action}; check it with `adk-rust agenda`"
    )]
    AgendaState {
        /// The item id.
        item: String,
        /// The item's state.
        state: crate::agenda::AgendaStateKind,
        /// The action refused, such as `approved`.
        action: &'static str,
    },

    /// A partition quota refuses the process (spec 3.2 §7.9).
    #[error(
        "partition {partition} is at its quota: {reason}; wait for its processes to finish or raise the quota"
    )]
    Quota {
        /// The partition.
        partition: String,
        /// Which quota and its value.
        reason: String,
    },

    /// An error from an `adk-core` or `adk-server` API the kernel composes.
    #[error(transparent)]
    Core(#[from] AdkError),

    /// A file or socket operation failed.
    #[error("kernel I/O failed: {0}")]
    Io(#[from] std::io::Error),

    /// A record could not be encoded or decoded.
    #[error("kernel record encoding failed: {0}")]
    Serde(#[from] serde_json::Error),
}

impl KernelError {
    /// The error's code: a `kernel.*` code, or the wrapped error's code for
    /// [`Core`](Self::Core).
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::KernelError;
    ///
    /// let error = KernelError::Frozen { reason: "incident 12".into() };
    /// assert_eq!(error.code(), adk_kernel::error::FROZEN_CODE);
    /// ```
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => NOT_FOUND_CODE,
            Self::InvalidState { .. } => INVALID_STATE_CODE,
            Self::Frozen { .. } => FROZEN_CODE,
            Self::Lifecycle { .. } => LIFECYCLE_CODE,
            Self::Config { .. } => CONFIG_CODE,
            Self::Store { .. } => STORE_CODE,
            Self::ControlPlane { .. } => CONTROL_PLANE_CODE,
            Self::Unauthorized { .. } => UNAUTHORIZED_CODE,
            Self::AgendaNotFound { .. } => AGENDA_NOT_FOUND_CODE,
            Self::AgendaState { .. } => AGENDA_STATE_CODE,
            Self::Quota { .. } => QUOTA_CODE,
            Self::Core(error) => error.code,
            Self::Io(_) => IO_CODE,
            Self::Serde(_) => SERDE_CODE,
        }
    }

    /// The error's category, which sets the HTTP status a control-plane route answers.
    pub fn category(&self) -> ErrorCategory {
        match self {
            Self::NotFound { .. } | Self::AgendaNotFound { .. } => ErrorCategory::NotFound,
            Self::InvalidState { .. } | Self::Lifecycle { .. } | Self::AgendaState { .. } => {
                ErrorCategory::Conflict
            }
            Self::Quota { .. } => ErrorCategory::ResourceExhausted,
            Self::Frozen { .. } | Self::Store { .. } | Self::ControlPlane { .. } => {
                ErrorCategory::Unavailable
            }
            Self::Config { .. } => ErrorCategory::InvalidInput,
            Self::Unauthorized { .. } => ErrorCategory::Unauthorized,
            Self::Core(error) => error.category,
            Self::Io(_) | Self::Serde(_) => ErrorCategory::Internal,
        }
    }
}

impl From<KernelError> for AdkError {
    fn from(error: KernelError) -> Self {
        match error {
            KernelError::Core(inner) => inner,
            other => AdkError::new(
                ErrorComponent::Server,
                other.category(),
                other.code(),
                other.to_string(),
            ),
        }
    }
}
