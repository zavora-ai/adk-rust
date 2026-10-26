//! `publish_delay`: the time a page the site drafts on its own waits before it can go live,
//! on top of the owner's approval (spec 3.5 §5 S10, §7.20 "Freshness … Propose for new
//! pages"; AWP 1.0's `[content] publish_delay`).
//!
//! [`PublishDelay`] is a tool policy on the back office: a new item a delayed job drafts —
//! `content_draft` or `translation_draft` under `freshness`, `seo` or `aeo` by default — must
//! carry `schedule.publishAt` at least the delay after now, and the policy refuses the draft
//! otherwise, saying the earliest time. Approving the draft then schedules it; the pipeline
//! publishes it when that time comes, so it waits out the delay and the approval both.
//! A draft that names no job is delayed when any job of its agent that covers the tool is.

use std::sync::Arc;

use adk_core::{Clock, PolicyDecision, SystemClock, ToolPolicy, ToolPolicyRequest};
use adk_harness::website::LadderCompiler;
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};

/// AWP 1.0's default delay.
pub const DEFAULT_PUBLISH_DELAY_HOURS: i64 = 24;

/// The jobs whose drafts wait out the delay by default: new pages the site writes from what
/// it observes.
pub const DELAYED_JOBS: [&str; 3] = ["freshness", "seo", "aeo"];

/// The drafting tools a delay applies to: they create items.
const DRAFTING: [&str; 2] = ["content_draft", "translation_draft"];

/// The publish delay as a back-office tool policy (spec 3.5 §5 S10).
///
/// # Example
///
/// ```rust
/// use adk_core::{PolicyDecision, ToolPolicy, ToolPolicyRequest};
/// use adk_harness::website::{LadderCompiler, SiteRef};
/// use adk_website::publish_delay::PublishDelay;
/// use serde_json::json;
///
/// # #[tokio::main(flavor = "current_thread")]
/// # async fn main() {
/// let site = SiteRef::new(uuid::Uuid::nil(), "bakery", "https://bakery.example");
/// let delay = PublishDelay::new(chrono::Duration::hours(24), LadderCompiler::new(site));
/// let now = json!({ "kind": "page", "slug": "faq", "language": "en", "job": "freshness" });
/// let decided = delay.evaluate(&ToolPolicyRequest::new("content_draft", now)).await;
/// assert!(matches!(decided, PolicyDecision::Deny { .. }));
/// # }
/// ```
pub struct PublishDelay {
    delay: Duration,
    jobs: Vec<String>,
    compiler: LadderCompiler,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for PublishDelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishDelay")
            .field("delay", &self.delay)
            .field("jobs", &self.jobs)
            .finish_non_exhaustive()
    }
}

impl PublishDelay {
    /// A delay of `delay` on [`DELAYED_JOBS`], whose agents and tools `compiler` binds.
    pub fn new(delay: Duration, compiler: LadderCompiler) -> Self {
        Self {
            delay,
            jobs: DELAYED_JOBS.iter().map(|job| (*job).to_string()).collect(),
            compiler,
            clock: Arc::new(SystemClock),
        }
    }

    /// Delays the drafts of `jobs` instead.
    #[must_use]
    pub fn with_jobs(mut self, jobs: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.jobs = jobs.into_iter().map(Into::into).collect();
        self
    }

    /// The clock "now" is read from.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// The delay.
    pub fn delay(&self) -> Duration {
        self.delay
    }

    /// Whether `request` drafts for a delayed job.
    fn delayed(&self, request: &ToolPolicyRequest) -> bool {
        match request.args.get("job").and_then(serde_json::Value::as_str) {
            Some(job) => self.jobs.iter().any(|delayed| delayed == job),
            None => self.compiler.jobs().iter().any(|binding| {
                binding.agent == request.agent_name
                    && binding.covers(&request.tool_name)
                    && self.jobs.contains(&binding.job)
            }),
        }
    }
}

#[async_trait]
impl ToolPolicy for PublishDelay {
    async fn evaluate(&self, request: &ToolPolicyRequest) -> PolicyDecision {
        if !DRAFTING.contains(&request.tool_name.as_str()) || !self.delayed(request) {
            return PolicyDecision::Allow;
        }
        let earliest = self.clock.now() + self.delay;
        let scheduled = request
            .args
            .pointer("/schedule/publishAt")
            .and_then(serde_json::Value::as_str)
            .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
            .map(|at| at.with_timezone(&Utc));
        // A minute of slack for the time the model took to write the call.
        match scheduled {
            Some(at) if at + Duration::minutes(1) >= earliest => PolicyDecision::Allow,
            _ => PolicyDecision::deny(format!(
                "a page this job drafts on its own waits out the site's publish delay of {} \
                 hours before it can go live, as well as the owner's approval: draft it with \
                 schedule.publishAt {} or later",
                self.delay.num_hours(),
                earliest.format("%Y-%m-%dT%H:%M:%SZ")
            )),
        }
    }

    fn policy_ref(&self) -> Option<String> {
        Some(format!("adk-website.publish-delay:{}", self.compiler.site().name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adk_harness::website::{JobBinding, SiteRef};
    use serde_json::json;

    fn delay(clock: Arc<dyn Clock>) -> PublishDelay {
        let site = SiteRef::new(uuid::Uuid::nil(), "bakery", "https://bakery.example");
        let compiler = LadderCompiler::new(site)
            .job(JobBinding::new("freshness", "growth", ["content_draft"]))
            .job(JobBinding::new("onboarding", "steward", ["content_*"]));
        PublishDelay::new(Duration::hours(24), compiler).with_clock(clock)
    }

    fn request(agent: &str, args: serde_json::Value) -> ToolPolicyRequest {
        let mut request = ToolPolicyRequest::new("content_draft", args);
        request.agent_name = agent.into();
        request
    }

    #[tokio::test]
    async fn a_gap_page_waits_out_the_delay_and_onboarding_does_not() {
        let now = Utc::now();
        let policy = delay(Arc::new(adk_core::ManualClock::new(now)));
        let page = |extra: serde_json::Value| {
            let mut args = json!({ "kind": "page", "slug": "wedding-cakes", "language": "en" });
            args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            args
        };
        let soon = (now + Duration::hours(2)).to_rfc3339();
        let later = (now + Duration::hours(24)).to_rfc3339();
        let refused =
            policy.evaluate(&request("growth", page(json!({ "job": "freshness" })))).await;
        assert_ne!(refused, PolicyDecision::Allow);
        let early = page(json!({ "job": "freshness", "schedule": { "publishAt": soon } }));
        assert_ne!(policy.evaluate(&request("growth", early)).await, PolicyDecision::Allow);
        let waits = page(json!({ "job": "freshness", "schedule": { "publishAt": later } }));
        assert_eq!(policy.evaluate(&request("growth", waits)).await, PolicyDecision::Allow);
        let unnamed = page(json!({}));
        assert_ne!(
            policy.evaluate(&request("growth", unnamed.clone())).await,
            PolicyDecision::Allow
        );
        assert_eq!(policy.evaluate(&request("steward", unnamed)).await, PolicyDecision::Allow);
        let onboarding = page(json!({ "job": "onboarding" }));
        assert_eq!(policy.evaluate(&request("steward", onboarding)).await, PolicyDecision::Allow);
    }
}
