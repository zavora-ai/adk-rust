//! Releasing an environment from code that cannot await: a `Drop`, or a call cancelled
//! mid-command.

use std::sync::Arc;
use std::time::Duration;

use super::client::SandboxClient;
use super::types::SessionHandle;

/// Longest a background release waits for the provider before giving up; the provider's own
/// lifetime bound (`ClientCapabilities::max_lifetime`) ends what this misses.
pub(crate) const RELEASE_TIMEOUT: Duration = Duration::from_secs(30);

/// Stops `handle` on the current Tokio runtime without waiting for it.
///
/// Outside a runtime nothing can run the stop; the environment then lives until the
/// provider's own lifetime bound ends it, which the warning says.
pub(crate) fn release_in_background(
    client: Arc<dyn SandboxClient>,
    handle: SessionHandle,
    reason: &'static str,
) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(
            sandbox.provider = client.provider(),
            sandbox.handle = handle.as_str(),
            reason,
            "no async runtime to release a sandbox environment; it lives until the provider's \
             own lifetime bound ends it"
        );
        return;
    };
    runtime.spawn(async move {
        match tokio::time::timeout(RELEASE_TIMEOUT, client.stop(&handle)).await {
            Ok(Ok(())) => tracing::info!(
                sandbox.provider = client.provider(),
                sandbox.handle = handle.as_str(),
                reason,
                "released a sandbox environment"
            ),
            Ok(Err(error)) => tracing::warn!(
                sandbox.provider = client.provider(),
                sandbox.handle = handle.as_str(),
                reason,
                error = %error,
                "could not release a sandbox environment"
            ),
            Err(_) => tracing::warn!(
                sandbox.provider = client.provider(),
                sandbox.handle = handle.as_str(),
                reason,
                timeout = ?RELEASE_TIMEOUT,
                "releasing a sandbox environment timed out"
            ),
        }
    });
}
