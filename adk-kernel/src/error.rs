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
/// Error code of [`KernelError::Service`].
pub const SERVICE_CODE: &str = "kernel.service";
/// Error code of [`KernelError::Io`].
pub const IO_CODE: &str = "kernel.io";
/// Error code of [`KernelError::Serde`].
pub const SERDE_CODE: &str = "kernel.serde";
/// Error code of a spawn the run store refuses because another run holds its concurrency
/// key and the run does not queue behind it.
pub const SPAWN_REJECTED_CODE: &str = "kernel.spawn_rejected";
/// Error code of a call a store does not support, such as attachments on a device store
/// that keeps none.
pub const UNSUPPORTED_CODE: &str = "kernel.unsupported";
/// Error code of a ledger row the ledger refuses (spec 3.2 §9.2 K-I19).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::error::LEDGER_INVALID_CODE, "kernel.ledger.invalid_row");
/// ```
pub const LEDGER_INVALID_CODE: &str = "kernel.ledger.invalid_row";
/// Error code of a crossing between partitions that no rule permits (spec 3.2 K-I15).
pub const CROSSING_DENIED_CODE: &str = "kernel.partition.crossing_denied";
/// Error code of a partition's memory call that names another partition's scope
/// (spec 3.2 K-I15).
pub const MEMORY_CROSSING_CODE: &str = "kernel.partition.memory_crossing";
/// Error code of a delete by query on a partition's memory, which would reach every
/// partition's scope.
pub const MEMORY_DELETE_CODE: &str = "kernel.partition.memory_delete";
/// Error code of an agent id the kernel does not keep (spec 3.3 §5.7).
pub const AGENT_NOT_FOUND_CODE: &str = "kernel.agent.not_found";
/// Error code of an agent definition that breaks a rule of spec 3.3 §5.7: its name, id,
/// instructions, folder or uses.
pub const AGENT_INVALID_CODE: &str = "kernel.agent.invalid";
/// Error code of an agent change made against an older version (spec 3.3 §5.7).
pub const AGENT_OUT_OF_DATE_CODE: &str = "kernel.agent.out_of_date";
/// Error code of an agent change the agent table refuses: a caller who is not a person, a
/// device, high risk without presence, more than an area or a suggesting agent allows
/// (spec 3.3 §5.7, S-I11, S-I12).
pub const AGENT_REFUSED_CODE: &str = "kernel.agent.refused";
/// Error code of an agent change applied with a digest that is not its preview's (spec 3.3
/// §5.7).
pub const AGENT_UNCONFIRMED_CODE: &str = "kernel.agent.unconfirmed";
/// Error code of `POST /kernel/unfreeze` refused over HTTP: resuming happens only in the web
/// home or on a paired local terminal (spec 3.3 S-I10).
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::error::RESUME_NOT_HERE_CODE, "kernel.resume.not_here");
/// ```
pub const RESUME_NOT_HERE_CODE: &str = "kernel.resume.not_here";
/// Error code of a decision on `POST /approvals/{id}/decide` that the caller's surface may not
/// give (spec 3.3 §7.2): it is answered in the home or on the kernel's own terminal.
///
/// # Example
///
/// ```rust
/// assert_eq!(adk_kernel::error::DECIDE_NOT_HERE_CODE, "surface.decide.not_here");
/// ```
pub const DECIDE_NOT_HERE_CODE: &str = "surface.decide.not_here";

/// Every code the kernel answers a control-plane call with: the codes a
/// [`ControlClient`](crate::control::ControlClient) keeps, and reports any other code as
/// [`CONTROL_PLANE_CODE`]. A test checks it against every `*_CODE` constant in the crate.
pub(crate) const CODES: &[&str] = &[
    NOT_FOUND_CODE,
    INVALID_STATE_CODE,
    FROZEN_CODE,
    LIFECYCLE_CODE,
    CONFIG_CODE,
    STORE_CODE,
    CONTROL_PLANE_CODE,
    UNAUTHORIZED_CODE,
    AGENDA_NOT_FOUND_CODE,
    AGENDA_STATE_CODE,
    QUOTA_CODE,
    SERVICE_CODE,
    IO_CODE,
    SERDE_CODE,
    SPAWN_REJECTED_CODE,
    UNSUPPORTED_CODE,
    LEDGER_INVALID_CODE,
    CROSSING_DENIED_CODE,
    MEMORY_CROSSING_CODE,
    MEMORY_DELETE_CODE,
    AGENT_NOT_FOUND_CODE,
    AGENT_INVALID_CODE,
    AGENT_OUT_OF_DATE_CODE,
    AGENT_REFUSED_CODE,
    AGENT_UNCONFIRMED_CODE,
    RESUME_NOT_HERE_CODE,
    DECIDE_NOT_HERE_CODE,
];

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

    /// Installing, removing or running the kernel as a service failed (spec 3.2 §7.12).
    #[error("kernel service: {reason}")]
    Service {
        /// What failed and how to fix it.
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
            Self::Service { .. } => SERVICE_CODE,
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
            Self::Frozen { .. }
            | Self::Store { .. }
            | Self::ControlPlane { .. }
            | Self::Service { .. } => ErrorCategory::Unavailable,
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    /// Every `.rs` file under the crate's `src`.
    fn sources() -> Vec<(PathBuf, String)> {
        let mut pending = vec![Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        let mut files = Vec::new();
        while let Some(path) = pending.pop() {
            if path.is_dir() {
                pending
                    .extend(std::fs::read_dir(&path).unwrap().map(|entry| entry.unwrap().path()));
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                files.push((path, text));
            }
        }
        files
    }

    /// The `index`th top-level argument of the call whose text follows its opening
    /// parenthesis, trimmed.
    fn argument(call: &str, index: usize) -> String {
        let (mut depth, mut current, mut in_string, mut escaped) = (0usize, 0, false, false);
        let mut text = String::new();
        for c in call.chars() {
            if in_string {
                in_string = c != '"' || escaped;
                escaped = c == '\\' && !escaped;
            } else {
                match c {
                    '"' => in_string = true,
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' if depth == 0 => break,
                    ')' | ']' | '}' => depth -= 1,
                    ',' if depth == 0 => {
                        current += 1;
                        continue;
                    }
                    _ => {}
                }
            }
            if current == index {
                text.push(c);
            }
        }
        text.trim().to_string()
    }

    // A code the client does not list reaches its callers as `kernel.control_plane`, so
    // every code the crate can answer with is a `*_CODE` constant listed in `CODES`.
    #[test]
    fn every_code_the_kernel_answers_with_is_listed() {
        let mut constants = Vec::new();
        let mut literals = Vec::new();
        for (path, text) in sources() {
            for line in text.lines() {
                if let Some((_, value)) = line.split_once("_CODE: &str = \"")
                    && let Some((code, _)) = value.split_once('"')
                    && (code.starts_with("kernel.") || code.starts_with("surface."))
                {
                    constants.push(code.to_string());
                }
            }
            for call in text.split("AdkError::new(").skip(1) {
                let code = argument(call, 2);
                if code.starts_with("\"kernel.") {
                    literals.push(format!("{}: {code}", path.display()));
                }
            }
        }
        assert!(constants.len() >= CODES.len(), "the scan found {constants:?}");
        let unlisted: Vec<_> =
            constants.iter().filter(|code| !CODES.contains(&code.as_str())).collect();
        assert!(unlisted.is_empty(), "add these codes to error::CODES: {unlisted:?}");
        assert!(
            literals.is_empty(),
            "name these codes as `*_CODE` constants in error.rs and list them in CODES: \
             {literals:?}"
        );
    }

    #[test]
    fn the_argument_scan_skips_nested_calls_and_strings() {
        let call = r#"ErrorComponent::Server, f(a, b), "a, (b", format!("{x}, y"))"#;
        assert_eq!(argument(call, 1), "f(a, b)");
        assert_eq!(argument(call, 2), r#""a, (b""#);
        assert_eq!(argument(call, 3), r#"format!("{x}, y")"#);
    }
}
