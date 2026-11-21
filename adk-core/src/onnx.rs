//! The process's one ONNX Runtime environment, shared by every crate that runs ONNX models.
//!
//! `ort` allows one [`Environment`] per process and gives no way to reach an existing one, so
//! `adk-audio`, `adk-rag` and `adk-vision` each take theirs from [`ort_environment`] rather than
//! building their own. An application that needs a custom environment (a global thread pool,
//! default execution providers, its own logger) builds it once and installs it with
//! [`set_ort_environment`] before it loads a model.
//!
//! # Example
//!
//! ```no_run
//! use adk_core::onnx::ort_environment;
//! use ort::session::Session;
//!
//! # fn main() -> ort::Result<()> {
//! let environment = ort_environment()?;
//! let session = Session::builder(&environment)?.commit_from_file("model.onnx")?;
//! # Ok(())
//! # }
//! ```

use std::sync::{Mutex, MutexGuard};

use ort::environment::Environment;
use ort::logging::LogLevel;

/// The message `ort` gives when a second environment is built in one process.
const ONE_PER_PROCESS: &str = "only one environment is allowed per process";

#[derive(Debug, thiserror::Error)]
#[error(
    "{0}: something outside ADK-Rust built the process's ONNX Runtime environment. Pass that \
     environment to `adk_core::onnx::set_ort_environment` before loading an ONNX model"
)]
struct ForeignEnvironment(#[source] ort::Error);

#[derive(Debug, thiserror::Error)]
#[error(
    "an ONNX Runtime environment is already installed. Call \
     `adk_core::onnx::set_ort_environment` once, before loading any ONNX model"
)]
struct AlreadyInstalled;

fn installed() -> MutexGuard<'static, Option<Environment>> {
    static INSTALLED: Mutex<Option<Environment>> = Mutex::new(None);
    // The guarded value is a handle that is set once and never left half-written.
    INSTALLED.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Returns the process's ONNX Runtime environment, building it with `ort`'s defaults on first
/// use. The environment lives until the process exits.
///
/// onnxruntime's warnings and errors reach `tracing`; call
/// [`Environment::set_log_level`] on the returned handle for its informational and verbose
/// records too.
///
/// # Example
///
/// ```no_run
/// use ort::AsPointer;
///
/// # fn main() -> ort::Result<()> {
/// let first = adk_core::onnx::ort_environment()?;
/// let second = adk_core::onnx::ort_environment()?;
/// assert_eq!(first.ptr(), second.ptr());
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns `ort`'s error when ONNX Runtime cannot create an environment, and an error naming
/// [`set_ort_environment`] when code outside ADK-Rust already built one.
pub fn ort_environment() -> ort::Result<Environment> {
    let mut installed = installed();
    if let Some(environment) = installed.as_ref() {
        return Ok(environment.clone());
    }
    let environment = ort::init().build().map_err(|error| {
        if error.message() == ONE_PER_PROCESS {
            ort::Error::wrap(ForeignEnvironment(error))
        } else {
            error
        }
    })?;
    // `ort` forwards every verbose record to `tracing`, and onnxruntime formats each one while
    // it builds a session; warnings and errors are what an application acts on.
    environment.set_log_level(LogLevel::Warning);
    tracing::debug!("onnx runtime environment created");
    Ok(installed.insert(environment).clone())
}

/// Installs an environment the application built, for every ONNX model ADK-Rust loads after.
///
/// # Example
///
/// ```no_run
/// # fn main() -> ort::Result<()> {
/// let environment = ort::init().with_name("my-app").build()?;
/// adk_core::onnx::set_ort_environment(environment)?;
/// # Ok(())
/// # }
/// ```
///
/// # Errors
///
/// Returns an error when an environment is already installed, by an earlier call or by a model
/// loaded before this one.
pub fn set_ort_environment(environment: Environment) -> ort::Result<()> {
    let mut installed = installed();
    if installed.is_some() {
        return Err(ort::Error::wrap(AlreadyInstalled));
    }
    *installed = Some(environment);
    Ok(())
}
