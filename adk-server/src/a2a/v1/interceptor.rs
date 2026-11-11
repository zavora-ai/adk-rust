//! Interceptors around A2A v1 operations, as a2a-go's call interceptors offer them.
//!
//! An interceptor sees every operation the server handles, on either binding or called
//! directly: `before` the operation it may change the params, answer the call itself, or
//! refuse it; `after` it may change the result, and on a stream (`SendStreamingMessage`,
//! `SubscribeToTask`) it sees each event. Params and results are the operation's JSON
//! shapes (the JSON-RPC `params` and `result`). Interceptors run `before` in the order they
//! were added and `after` in reverse, as layered middleware does.
//!
//! ```rust,ignore
//! struct Audit;
//!
//! #[async_trait::async_trait]
//! impl CallInterceptor for Audit {
//!     async fn before(&self, call: &CallContext, params: &mut serde_json::Value)
//!         -> Result<Option<serde_json::Value>, A2aError> {
//!         tracing::info!(method = call.method, user = ?call.caller.as_ref().map(|c| &c.user_id));
//!         Ok(None)
//!     }
//! }
//!
//! let server = A2aV1Server::builder(runner_config, base_url).interceptor(Arc::new(Audit)).build();
//! ```

use async_trait::async_trait;
use serde_json::Value;

use super::error::A2aError;
use crate::auth_bridge::RequestContext;

/// The operation an interceptor sees.
#[derive(Debug, Clone)]
pub struct CallContext {
    /// The A2A method: `SendMessage`, `SendStreamingMessage`, `GetTask`, `CancelTask`,
    /// `ListTasks`, `SubscribeToTask`, `CreateTaskPushNotificationConfig`,
    /// `GetTaskPushNotificationConfig`, `ListTaskPushNotificationConfigs`, or
    /// `DeleteTaskPushNotificationConfig`.
    pub method: &'static str,
    /// The authenticated caller, if any.
    pub caller: Option<RequestContext>,
    /// The extensions the caller asked for (`A2A-Extensions`).
    pub extensions: Vec<String>,
}

/// Runs around each A2A v1 operation; see the [module documentation](self). Both methods
/// default to passing the call through.
#[async_trait]
pub trait CallInterceptor: Send + Sync {
    /// Before the operation: may change `params`, or return a result that answers the call
    /// without running it. An error refuses the call.
    ///
    /// # Errors
    ///
    /// Returns the error the caller receives instead of the operation's result.
    async fn before(
        &self,
        _call: &CallContext,
        _params: &mut Value,
    ) -> Result<Option<Value>, A2aError> {
        Ok(None)
    }

    /// After the operation: may change its `result` (each event, for a stream). An error
    /// replaces the result.
    ///
    /// # Errors
    ///
    /// Returns the error the caller receives instead of the result.
    async fn after(&self, _call: &CallContext, _result: &mut Value) -> Result<(), A2aError> {
        Ok(())
    }
}
