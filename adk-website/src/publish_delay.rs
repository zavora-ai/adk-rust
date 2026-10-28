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
//! With [`PublishDelay::with_charters`], the charter in force sets the delay
//! (`publishDelayHours`, spec 3.5 §7.6), and the configured delay applies when it sets none.

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
    charters: Option<Arc<dyn adk_kernel::charter::CharterStore>>,
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
            charters: None,
            clock: Arc::new(SystemClock),
        }
    }

    /// Reads the delay from the site's charter in force in `charters` (spec 3.5 §5 S10,
    /// §7.6): its `publishDelayHours`, else the delay given to [`new`](Self::new).
    #[must_use]
    pub fn with_charters(mut self, charters: Arc<dyn adk_kernel::charter::CharterStore>) -> Self {
        self.charters = Some(charters);
        self
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

    /// The delay given to [`new`](Self::new), which applies when the charter sets none.
    pub fn delay(&self) -> Duration {
        self.delay
    }

    /// The delay in force now: the charter's `publishDelayHours` when the charter in force
    /// sets it, else [`delay`](Self::delay).
    pub async fn current_delay(&self) -> Duration {
        let Some(charters) = &self.charters else { return self.delay };
        match charters.current(self.compiler.site().id).await {
            Ok(current) => current
                .and_then(|version| version.charter.publish_delay_hours)
                .map_or(self.delay, |hours| Duration::hours(i64::from(hours))),
            Err(error) => {
                tracing::warn!(error = %error, "the charter's publish delay could not be read; the default applies");
                self.delay
            }
        }
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
        let delay = self.current_delay().await;
        let earliest = self.clock.now() + delay;
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
                delay.num_hours(),
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

    /// The charter in force sets the delay (spec 3.5 §5 S10, §7.6); the configured one applies
    /// while it sets none.
    #[tokio::test]
    async fn the_charters_publish_delay_replaces_the_default() {
        use adk_core::principal::SubjectRef;
        use adk_kernel::charter::{
            CharterStore, CommitDecision, InMemoryCharterStore, RoleApproval,
        };

        let now = Utc::now();
        let charters = Arc::new(InMemoryCharterStore::new());
        let policy =
            delay(Arc::new(adk_core::ManualClock::new(now))).with_charters(charters.clone());
        assert_eq!(policy.current_delay().await, Duration::hours(24));

        let site = SiteRef::new(uuid::Uuid::nil(), "bakery", "https://bakery.example");
        let kind = crate::kinds::template(awp_types::SiteKind::Business);
        let team = crate::team::SiteTeam::new(site, &kind);
        let inputs = crate::kinds::CharterInputs::new(
            uuid::Uuid::nil(),
            "Juniper Bakery",
            "amina",
            "Bread.",
            vec!["en".parse().unwrap()],
            "KES",
        );
        let mut charter = kind.draft_charter(&team, &inputs);
        charter.publish_delay_hours = Some(48);
        let decision = CommitDecision::new("dec-1", now).with_approval(RoleApproval::new(
            awp_types::Role::Owner,
            SubjectRef::human("amina"),
            now,
        ));
        charters.commit(charter, decision).await.unwrap();
        assert_eq!(policy.current_delay().await, Duration::hours(48));

        let day = (now + Duration::hours(24)).to_rfc3339();
        let args = json!({ "kind": "page", "slug": "faq", "language": "en", "job": "freshness", "schedule": { "publishAt": day } });
        let decided = policy.evaluate(&request("growth", args)).await;
        assert!(
            matches!(&decided, PolicyDecision::Deny { reason, .. } if reason.contains("48 hours")),
            "{decided:?}"
        );
    }
}
