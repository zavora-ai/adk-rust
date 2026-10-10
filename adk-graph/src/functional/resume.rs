//! Restores the point an `#[entrypoint]` run resumes from.

use crate::checkpoint::Checkpointer;
use crate::error::{GraphError, Result};
use crate::node::ExecutionConfig;
use crate::state::State;

use super::execution_log::ExecutionLog;

/// Loads the state and execution log of the checkpoint `config.resume_from` names.
///
/// Returns `None` when `config.resume_from` is unset, so the run starts fresh. A
/// checkpoint that carries no execution log restores an empty one.
///
/// Called by the `#[entrypoint]` expansion; not part of the public API.
///
/// # Errors
///
/// Returns [`GraphError::CheckpointError`] when `config.thread_id` has no
/// checkpoint with the requested id, including when that id belongs to another
/// thread, or when the checkpoint's execution log does not deserialize. Each
/// refuses the resume rather than starting over, which would run completed tasks
/// again.
#[doc(hidden)]
pub async fn load_resume_point(
    checkpointer: &dyn Checkpointer,
    config: &ExecutionConfig,
) -> Result<Option<(State, ExecutionLog)>> {
    let Some(checkpoint_id) = config.resume_from.as_deref() else {
        return Ok(None);
    };
    let thread_id = &config.thread_id;

    // Another thread's checkpoint is reported exactly like a missing one, so a
    // caller cannot resume, or probe for, state outside its own thread.
    let checkpoint = checkpointer
        .load_by_id(checkpoint_id)
        .await?
        .filter(|checkpoint| checkpoint.thread_id == *thread_id)
        .ok_or_else(|| {
            GraphError::CheckpointError(format!(
                "cannot resume thread '{thread_id}': it has no checkpoint with id \
                 '{checkpoint_id}'. Pass an id that `Checkpointer::load` or \
                 `Checkpointer::list` returns for this thread"
            ))
        })?;

    let log = match checkpoint.metadata.get("execution_log") {
        Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
            GraphError::CheckpointError(format!(
                "checkpoint '{checkpoint_id}' holds an execution log that does not \
                 deserialize: {error}"
            ))
        })?,
        None => ExecutionLog::new(),
    };

    Ok(Some((checkpoint.state, log)))
}
