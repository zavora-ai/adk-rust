//! What can go wrong, with what to do about it.

use std::path::PathBuf;

/// The shell's errors (spec 3.3 §5.5). Each says what to do next.
#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    /// `--server` is not a URL the tray can use.
    #[error("`{value}` is not a server address: {reason}. Pass --server http://127.0.0.1:8080")]
    BadServer {
        /// What was given.
        value: String,
        /// Why it does not work.
        reason: String,
    },

    /// The server is on another machine.
    #[error(
        "the tray reaches a kernel on this computer only, not `{host}`: start `adk-rust up` here \
         and pass --server http://127.0.0.1:<port>"
    )]
    NotLoopback {
        /// The host that was given.
        host: String,
    },

    /// The user's `harness.toml` could not be read.
    #[error("could not read [kernel] bind from {path}: {reason}. Fix the file or pass --server")]
    Config {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        reason: String,
    },

    /// The kernel could not be reached.
    #[error("can't reach your agents at {origin}: {source}. Start them with `adk-rust up`")]
    Unreachable {
        /// The server.
        origin: String,
        /// The transport error.
        #[source]
        source: reqwest::Error,
    },

    /// The kernel asked for its token.
    #[error(
        "the kernel at {origin} needs its token: start the tray with --token or set ADK_SERVE_TOKEN"
    )]
    SignIn {
        /// The server.
        origin: String,
    },

    /// The kernel refused the request.
    #[error("the kernel refused: {message} ({status})")]
    Refused {
        /// The HTTP status.
        status: u16,
        /// The problem code, such as `surface.operator_only`.
        code: Option<String>,
        /// The problem's message.
        message: String,
    },
}

/// The shell's results.
pub type Result<T> = std::result::Result<T, DesktopError>;
