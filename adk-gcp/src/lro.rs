//! Long-running-operation polling with identity pinning.

use crate::client::GcpHttpClient;
use crate::error::truncate_for_error;
use crate::resource::is_scoped_resource_name;
use adk_core::Result;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tokio::time::Instant;

const DEFAULT_POLL_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_INITIAL_DELAY: Duration = Duration::from_millis(100);
const DEFAULT_MAX_DELAY: Duration = Duration::from_secs(2);

/// A `google.longrunning.Operation` wire value.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Operation {
    /// The server-assigned operation resource name.
    pub name: String,
    /// Whether the operation has reached a terminal state.
    #[serde(default)]
    pub done: bool,
    /// The error result, set only on failed terminal operations.
    #[serde(default)]
    pub error: Option<OperationError>,
    /// The success result, set only on completed terminal operations.
    #[serde(default)]
    pub response: Option<Value>,
}

/// The `google.rpc.Status` error carried by a failed operation.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OperationError {
    /// The gRPC status code.
    #[serde(default)]
    pub code: i64,
    /// The developer-facing error message.
    #[serde(default)]
    pub message: String,
}

/// Polls long-running operations to completion with capped backoff.
///
/// The poller pins the operation identity — a poll can never silently
/// follow a different operation — and validates the operation name against
/// the caller's project and location, so a compromised or buggy server
/// cannot redirect polling elsewhere.
///
/// # Example
///
/// ```rust,no_run
/// use adk_gcp::LroPoller;
/// use std::time::Duration;
///
/// // Engine provisioning takes minutes; the deploy client widens the deadline.
/// let poller = LroPoller::new()
///     .with_poll_timeout(Duration::from_secs(900))
///     .with_max_delay(Duration::from_secs(10));
/// # let _ = poller;
/// ```
#[derive(Debug, Clone)]
pub struct LroPoller {
    poll_timeout: Duration,
    initial_delay: Duration,
    max_delay: Duration,
}

impl Default for LroPoller {
    fn default() -> Self {
        Self::new()
    }
}

impl LroPoller {
    /// Creates a poller with the workspace defaults: 120 s deadline,
    /// 100 ms initial delay, 2 s delay cap.
    pub fn new() -> Self {
        Self {
            poll_timeout: DEFAULT_POLL_TIMEOUT,
            initial_delay: DEFAULT_INITIAL_DELAY,
            max_delay: DEFAULT_MAX_DELAY,
        }
    }

    /// Sets the overall completion deadline.
    #[must_use]
    pub fn with_poll_timeout(mut self, timeout: Duration) -> Self {
        self.poll_timeout = timeout;
        self
    }

    /// Sets the delay before the first poll.
    #[must_use]
    pub fn with_initial_delay(mut self, delay: Duration) -> Self {
        self.initial_delay = delay;
        self
    }

    /// Sets the backoff delay cap.
    #[must_use]
    pub fn with_max_delay(mut self, delay: Duration) -> Self {
        self.max_delay = delay;
        self
    }

    /// Polls the operation in `initial` to completion.
    ///
    /// `operation_kind` names the operation in error messages (e.g.
    /// `"memories generate"`). When `require_response` is set, a completed
    /// operation without a `response` payload is an error.
    ///
    /// # Errors
    ///
    /// Returns an error when the initial value is not a valid operation,
    /// the operation name is outside `project_id`/`location`, a poll
    /// changes the operation identity, the operation completes with an
    /// error result, or the deadline passes before completion.
    pub async fn wait_for_operation(
        &self,
        client: &GcpHttpClient,
        initial: Value,
        operation_kind: &str,
        require_response: bool,
        project_id: &str,
        location: &str,
    ) -> Result<Option<Value>> {
        let errors = client.errors();
        let mut operation = parse_operation(client, initial, operation_kind)?;
        if !is_scoped_resource_name(&operation.name, project_id, location) {
            let name = truncate_for_error(&operation.name);
            return Err(errors.invalid_response(format!(
                "{} operation name '{name}' does not belong to projects/{project_id}/locations/{location}",
                errors.subject(),
            )));
        }
        let operation_name = operation.name.clone();
        let deadline = Instant::now() + self.poll_timeout;
        let mut delay = self.initial_delay;

        loop {
            if operation.done {
                if let Some(error) = operation.error {
                    return Err(errors.operation_error(
                        operation_kind,
                        &operation_name,
                        error.code,
                        &error.message,
                    ));
                }
                if require_response && operation.response.is_none() {
                    return Err(errors.invalid_response(format!(
                        "{} {operation_kind} operation '{operation_name}' completed without a response; retry the request and inspect the operation in Google Cloud",
                        errors.subject(),
                    )));
                }
                return Ok(operation.response);
            }
            if Instant::now() >= deadline {
                return Err(self.deadline_error(client, operation_kind, &operation_name));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let poll = async {
                let request = client.request(Method::GET, &operation_name).await?;
                client.send_value(request).await
            };
            let value = tokio::time::timeout(remaining, poll)
                .await
                .map_err(|_| self.deadline_error(client, operation_kind, &operation_name))??;
            let next = parse_operation(client, value, operation_kind)?;
            if next.name != operation_name {
                return Err(errors.invalid_response(format!(
                    "{} {operation_kind} poll changed operation identity from '{operation_name}' to '{}'; refusing to follow a different operation",
                    errors.subject(),
                    truncate_for_error(&next.name),
                )));
            }
            operation = next;
            if !operation.done {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    continue;
                }
                tokio::time::sleep(delay.min(remaining)).await;
                delay = delay.saturating_mul(2).min(self.max_delay);
            }
        }
    }

    fn deadline_error(
        &self,
        client: &GcpHttpClient,
        operation_kind: &str,
        operation_name: &str,
    ) -> adk_core::AdkError {
        let errors = client.errors();
        errors.timeout(format!(
            "{} {operation_kind} operation '{operation_name}' did not complete within {} seconds; inspect the operation in Google Cloud before retrying",
            errors.subject(),
            self.poll_timeout.as_secs_f64(),
        ))
    }
}

/// Parses an operation wire value, rejecting inconsistent shapes.
fn parse_operation(
    client: &GcpHttpClient,
    value: Value,
    operation_kind: &str,
) -> Result<Operation> {
    let errors = client.errors();
    let operation: Operation = serde_json::from_value(value).map_err(|error| {
        let error = truncate_for_error(&error.to_string());
        errors.invalid_response(format!(
            "failed to parse {} {operation_kind} operation: {error}",
            errors.subject(),
        ))
    })?;
    if operation.name.trim().is_empty() {
        return Err(errors.invalid_response(format!(
            "{} {operation_kind} response did not contain an operation name",
            errors.subject(),
        )));
    }
    if operation.error.is_some() && operation.response.is_some() {
        return Err(errors.invalid_response(format!(
            "{} {operation_kind} operation '{}' contains both error and response results",
            errors.subject(),
            truncate_for_error(&operation.name),
        )));
    }
    if !operation.done && (operation.error.is_some() || operation.response.is_some()) {
        return Err(errors.invalid_response(format!(
            "{} {operation_kind} operation '{}' contains a terminal result while done is false",
            errors.subject(),
            truncate_for_error(&operation.name),
        )));
    }
    Ok(operation)
}
