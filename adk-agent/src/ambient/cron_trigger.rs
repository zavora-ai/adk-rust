use std::str::FromStr;

use adk_core::{AdkError, Result};
use async_trait::async_trait;
use cron::Schedule;
use futures::stream::BoxStream;
use tokio::time::sleep;

use super::event_source::{EventSource, TriggerEvent};

/// Emits trigger events on a cron schedule.
///
/// Uses the `cron` crate for expression parsing and next-tick calculation.
///
/// # Example
///
/// ```rust,ignore
/// use adk_agent::ambient::CronTrigger;
///
/// // Fire every minute
/// let trigger = CronTrigger::new("0 * * * * *")?;
/// ```
pub struct CronTrigger {
    expression: String,
    schedule: Schedule,
    name: String,
}

impl CronTrigger {
    /// Create a new cron trigger from a cron expression.
    ///
    /// Returns an error if the expression is invalid.
    ///
    /// # Errors
    ///
    /// Returns `AdkError::Agent` with the parse error if the expression is invalid.
    pub fn new(expression: &str) -> Result<Self> {
        let schedule = Schedule::from_str(expression)
            .map_err(|e| AdkError::agent(format!("invalid cron expression: {e}")))?;

        Ok(Self {
            expression: expression.to_string(),
            schedule,
            name: format!("cron:{expression}"),
        })
    }
}

#[async_trait]
impl EventSource for CronTrigger {
    fn name(&self) -> &str {
        &self.name
    }

    async fn subscribe(&self) -> Result<BoxStream<'static, TriggerEvent>> {
        let schedule = self.schedule.clone();
        let source_name = self.name.clone();
        let expression = self.expression.clone();

        let stream = async_stream::stream! {
            loop {
                let now = chrono::Utc::now();
                let next = schedule.upcoming(chrono::Utc).next();

                let Some(next_tick) = next else {
                    // No more upcoming ticks — schedule is exhausted
                    break;
                };

                let duration = (next_tick - now).to_std().unwrap_or_default();
                sleep(duration).await;

                yield TriggerEvent {
                    source: source_name.clone(),
                    payload: serde_json::json!({
                        "expression": expression,
                        "tick": chrono::Utc::now().to_rfc3339(),
                    }),
                    // A schedule has no caller.
                    principal: None,
                };
            }
        };

        Ok(Box::pin(stream))
    }
}

impl std::fmt::Debug for CronTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CronTrigger").field("expression", &self.expression).finish()
    }
}
