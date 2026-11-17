//! Which finished runs online evaluation samples, and the trace it scores.

use std::collections::HashMap;

use adk_core::{Content, Event, GovernanceControl};
use serde::{Deserialize, Serialize};

use crate::error::{EvalError, Result};

/// How a run ended, as online evaluation classifies it from the run's events.
///
/// # Example
///
/// ```
/// use adk_core::{Content, Event};
/// use adk_eval::online::RunOutcome;
///
/// let mut answer = Event::new("inv-1");
/// answer.llm_response.content = Some(Content::new("model").with_text("Sunny."));
/// assert_eq!(RunOutcome::of(&[answer.clone()], false), RunOutcome::Completed);
///
/// answer.llm_response.error_code = Some("model.unavailable".to_string());
/// assert_eq!(RunOutcome::of(&[answer], false), RunOutcome::Errored);
/// assert_eq!(RunOutcome::of(&[], false), RunOutcome::Incomplete);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunOutcome {
    /// The run ended on a final response and no event carried an error.
    Completed,
    /// An event carried an error code or message, such as a budget refusal or a model error.
    Errored,
    /// The run ended without a final response: a stream error, a held tool call, or an
    /// interrupt.
    Incomplete,
    /// The run was cancelled.
    Cancelled,
}

impl RunOutcome {
    /// Classifies a run from its complete (non-partial) events and whether it was cancelled.
    pub fn of(events: &[Event], cancelled: bool) -> Self {
        if cancelled {
            return Self::Cancelled;
        }
        let complete: Vec<&Event> =
            events.iter().filter(|event| !event.llm_response.partial).collect();
        if complete.iter().any(|event| {
            event.llm_response.error_code.is_some() || event.llm_response.error_message.is_some()
        }) {
            return Self::Errored;
        }
        match complete.last() {
            Some(last) if last.is_final_response() => Self::Completed,
            _ => Self::Incomplete,
        }
    }
}

/// A request-metadata condition a run must meet to be sampled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MetadataFilter {
    /// The metadata key ([`RunConfig::request_metadata`](adk_core::RunConfig::request_metadata)).
    pub key: String,
    /// The value the key must have; `None` accepts any value of a present key.
    pub value: Option<String>,
}

/// Which finished runs online evaluation samples.
///
/// A run is sampled when its agent, request metadata and outcome pass the filters (an empty
/// filter passes everything) and its sample point falls under the rate. The sample point is
/// a deterministic hash of the seed and the run's invocation id, so the same seed samples the
/// same runs in every process and on every replay, and a different seed samples a different
/// set of the same size.
///
/// # Example
///
/// ```
/// use adk_eval::online::{RunOutcome, SamplingPolicy};
///
/// let policy = SamplingPolicy::rate(0.25)?
///     .with_seed(42)
///     .agent("support")
///     .tag("tier", Some("gold"))
///     .outcome(RunOutcome::Completed);
/// let sampled = (0..1_000)
///     .filter(|run| policy.sample_point(&format!("inv-{run}")) < 0.25)
///     .count();
/// assert!((200..300).contains(&sampled));
/// assert_eq!(policy.sample_point("inv-7"), policy.sample_point("inv-7"));
/// # Ok::<(), adk_eval::EvalError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SamplingPolicy {
    /// Fraction of runs to sample, from 0.0 (none) to 1.0 (every run).
    pub rate: f64,
    /// Seed of the sample point hash.
    pub seed: u64,
    /// Root agents whose runs may be sampled; empty means every agent.
    pub agents: Vec<String>,
    /// Request-metadata conditions a run must all meet; empty means any metadata.
    pub metadata: Vec<MetadataFilter>,
    /// Outcomes a run must end with; empty means any outcome.
    pub outcomes: Vec<RunOutcome>,
}

impl SamplingPolicy {
    /// Samples `rate` of runs, from 0.0 to 1.0, with seed 0 and no filters.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::ConfigError`] when `rate` is not between 0.0 and 1.0.
    pub fn rate(rate: f64) -> Result<Self> {
        if !(0.0..=1.0).contains(&rate) {
            return Err(EvalError::ConfigError(format!(
                "the online evaluation sample rate must be between 0.0 and 1.0, got {rate}"
            )));
        }
        Ok(Self { rate, seed: 0, agents: Vec::new(), metadata: Vec::new(), outcomes: Vec::new() })
    }

    /// Sets the seed of the sample point hash.
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Admits runs of the agent named `name` (repeatable).
    pub fn agent(mut self, name: impl Into<String>) -> Self {
        self.agents.push(name.into());
        self
    }

    /// Requires request metadata `key`, with `value` when one is given (repeatable).
    pub fn tag(mut self, key: impl Into<String>, value: Option<&str>) -> Self {
        self.metadata.push(MetadataFilter { key: key.into(), value: value.map(str::to_string) });
        self
    }

    /// Admits runs that end with `outcome` (repeatable).
    pub fn outcome(mut self, outcome: RunOutcome) -> Self {
        self.outcomes.push(outcome);
        self
    }

    /// The run's deterministic sample point in `[0, 1)`.
    pub fn sample_point(&self, invocation_id: &str) -> f64 {
        // FNV-1a over the seed and the id, then a SplitMix64 finaliser to spread the bits;
        // both are fixed, unlike `DefaultHasher`, so the point is stable across releases.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self.seed.to_le_bytes().iter().chain(invocation_id.as_bytes()) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        hash = (hash ^ (hash >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        hash = (hash ^ (hash >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        hash ^= hash >> 31;
        // The top 53 bits make an exactly representable fraction.
        (hash >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Whether a run of `agent` with `metadata` and `invocation_id` is sampled, before its
    /// outcome is known.
    pub fn admits_run(
        &self,
        agent: &str,
        metadata: &HashMap<String, String>,
        invocation_id: &str,
    ) -> bool {
        self.rate > 0.0
            && (self.agents.is_empty() || self.agents.iter().any(|name| name == agent))
            && self.metadata.iter().all(|filter| match (metadata.get(&filter.key), &filter.value) {
                (Some(actual), Some(expected)) => actual == expected,
                (Some(_), None) => true,
                (None, _) => false,
            })
            && self.sample_point(invocation_id) < self.rate
    }

    /// Whether a run that ended with `outcome` is evaluated.
    pub fn admits_outcome(&self, outcome: RunOutcome) -> bool {
        self.outcomes.is_empty() || self.outcomes.contains(&outcome)
    }

    /// Whether `trace` is sampled: [`admits_run`](Self::admits_run) and
    /// [`admits_outcome`](Self::admits_outcome).
    pub fn admits(&self, trace: &RunTrace) -> bool {
        self.admits_run(&trace.agent_name, &trace.metadata, &trace.invocation_id)
            && self.admits_outcome(trace.outcome)
    }
}

/// A finished run as online evaluation scores it.
///
/// The [`plugin`](crate::online::OnlineEvaluator::plugin) builds one from each sampled run;
/// an application that collects runs some other way builds one with [`new`](Self::new) and
/// passes it to [`submit`](crate::online::OnlineEvaluator::submit).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunTrace {
    /// The runner's application name.
    pub app_name: String,
    /// The session the run belongs to.
    pub session_id: String,
    /// The run's invocation id.
    pub invocation_id: String,
    /// The agent the run started.
    pub agent_name: String,
    /// The run's request metadata.
    pub metadata: HashMap<String, String>,
    /// The user message that started the run.
    pub user_content: Content,
    /// The run's complete (non-partial) events, in order.
    pub events: Vec<Event>,
    /// How the run ended.
    pub outcome: RunOutcome,
    /// The whole session's conversation, for thread-level criteria; empty otherwise.
    pub thread: Vec<Content>,
    /// The run's kill switches, checked again before the run is evaluated.
    pub governance: Vec<GovernanceControl>,
    /// When the run finished.
    pub finished_at: chrono::DateTime<chrono::Utc>,
}

impl RunTrace {
    /// A trace of a run that has finished now, its outcome classified from `events`
    /// ([`RunOutcome::of`]).
    ///
    /// # Example
    ///
    /// ```
    /// use adk_core::{Content, Event};
    /// use adk_eval::online::{RunOutcome, RunTrace};
    ///
    /// let mut answer = Event::new("inv-1");
    /// answer.llm_response.content = Some(Content::new("model").with_text("Sunny."));
    /// let trace = RunTrace::new(
    ///     "weather-app",
    ///     "session-1",
    ///     "inv-1",
    ///     "weather_agent",
    ///     Content::new("user").with_text("Weather in Nairobi?"),
    ///     vec![answer],
    /// )
    /// .with_metadata("tier", "gold");
    /// assert_eq!(trace.outcome, RunOutcome::Completed);
    /// ```
    pub fn new(
        app_name: impl Into<String>,
        session_id: impl Into<String>,
        invocation_id: impl Into<String>,
        agent_name: impl Into<String>,
        user_content: Content,
        events: Vec<Event>,
    ) -> Self {
        let events: Vec<Event> =
            events.into_iter().filter(|event| !event.llm_response.partial).collect();
        Self {
            app_name: app_name.into(),
            session_id: session_id.into(),
            invocation_id: invocation_id.into(),
            agent_name: agent_name.into(),
            metadata: HashMap::new(),
            user_content,
            outcome: RunOutcome::of(&events, false),
            events,
            thread: Vec::new(),
            governance: Vec::new(),
            finished_at: chrono::Utc::now(),
        }
    }

    /// Adds a request-metadata entry.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Sets how the run ended, overriding the classification from its events.
    pub fn with_outcome(mut self, outcome: RunOutcome) -> Self {
        self.outcome = outcome;
        self
    }

    /// Sets the whole session's conversation, for thread-level criteria.
    pub fn with_thread(mut self, thread: Vec<Content>) -> Self {
        self.thread = thread;
        self
    }

    /// Adds a kill switch that stops the run's evaluation while it is frozen.
    pub fn with_governance(mut self, control: GovernanceControl) -> Self {
        self.governance.push(control);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_points_are_deterministic_per_seed_and_uniform() {
        let policy = SamplingPolicy::rate(0.3).unwrap().with_seed(11);
        let ids: Vec<String> = (0..20_000).map(|run| format!("e-{run:08x}")).collect();
        let sampled: Vec<&String> =
            ids.iter().filter(|id| policy.admits_run("a", &HashMap::new(), id)).collect();
        let again: Vec<&String> =
            ids.iter().filter(|id| policy.admits_run("a", &HashMap::new(), id)).collect();
        assert_eq!(sampled, again);
        let share = sampled.len() as f64 / ids.len() as f64;
        assert!((share - 0.3).abs() < 0.015, "sampled {share}");

        let reseeded = policy.clone().with_seed(12);
        let other: Vec<&String> =
            ids.iter().filter(|id| reseeded.admits_run("a", &HashMap::new(), id)).collect();
        assert_ne!(sampled, other);
        let overlap = sampled.iter().filter(|id| other.contains(id)).count() as f64;
        // Independent seeds overlap in about rate² of the runs.
        assert!((overlap / ids.len() as f64 - 0.09).abs() < 0.015, "overlap {overlap}");
    }

    #[test]
    fn a_rate_of_zero_samples_nothing_and_one_samples_everything() {
        let none = SamplingPolicy::rate(0.0).unwrap();
        let all = SamplingPolicy::rate(1.0).unwrap();
        for run in 0..1_000 {
            let id = format!("inv-{run}");
            assert!(!none.admits_run("a", &HashMap::new(), &id));
            assert!(all.admits_run("a", &HashMap::new(), &id));
        }
        assert!(SamplingPolicy::rate(1.01).is_err());
        assert!(SamplingPolicy::rate(-0.1).is_err());
        assert!(SamplingPolicy::rate(f64::NAN).is_err());
    }

    #[test]
    fn filters_select_agents_metadata_and_outcomes() {
        let policy = SamplingPolicy::rate(1.0)
            .unwrap()
            .agent("support")
            .tag("tier", Some("gold"))
            .tag("channel", None)
            .outcome(RunOutcome::Completed)
            .outcome(RunOutcome::Errored);
        let metadata = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        let gold = metadata(&[("tier", "gold"), ("channel", "web")]);

        assert!(policy.admits_run("support", &gold, "inv"));
        assert!(!policy.admits_run("billing", &gold, "inv"));
        assert!(!policy.admits_run(
            "support",
            &metadata(&[("tier", "silver"), ("channel", "web")]),
            "inv"
        ));
        assert!(!policy.admits_run("support", &metadata(&[("tier", "gold")]), "inv"));
        assert!(policy.admits_outcome(RunOutcome::Errored));
        assert!(!policy.admits_outcome(RunOutcome::Incomplete));
        assert!(SamplingPolicy::rate(1.0).unwrap().admits_outcome(RunOutcome::Cancelled));
    }

    #[test]
    fn outcomes_follow_the_events() {
        let mut call = Event::new("inv");
        let mut content = Content::new("model");
        content.parts.push(adk_core::Part::FunctionCall {
            name: "lookup".to_string(),
            args: serde_json::json!({}),
            id: None,
            thought_signature: None,
        });
        call.llm_response.content = Some(content);
        let mut answer = Event::new("inv");
        answer.llm_response.content = Some(Content::new("model").with_text("done"));

        assert_eq!(RunOutcome::of(&[call.clone()], false), RunOutcome::Incomplete);
        assert_eq!(RunOutcome::of(&[call, answer.clone()], false), RunOutcome::Completed);
        assert_eq!(RunOutcome::of(&[answer], true), RunOutcome::Cancelled);
    }
}
