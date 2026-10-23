//! Observations and the state a baseline pass carries between days (spec 3.2 §7.10,
//! §9.2 K-I20).
//!
//! An observation is offered to the person as a question in neutral language, never a
//! diagnosis. It names the dimensions that changed, which way and since when, and the
//! comparison windows; it carries no value that identifies content (§9.3).

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Digest as _;

use crate::partition::PartitionId;

/// Which way a dimension moved from its baseline (spec 3.2 §7.10).
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::Direction;
///
/// assert_eq!(serde_json::to_value(Direction::Higher).unwrap(), "higher");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum Direction {
    /// Above the baseline: later, more or longer.
    Higher,
    /// Below the baseline: earlier, fewer or shorter.
    Lower,
}

/// One dimension an observation names (spec 3.2 §7.10).
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::{Direction, ObservedDimension};
/// use chrono::NaiveDate;
///
/// let since = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
/// let dimension = ObservedDimension::new("activity.last_minute", Direction::Higher, since, 10);
/// assert_eq!(dimension.days, 10);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ObservedDimension {
    /// The dimension, such as `activity.last_minute` or `tool_call.count`.
    pub dimension: String,
    /// Which way it moved.
    pub direction: Direction,
    /// The first day of its run of deviating days.
    pub since: NaiveDate,
    /// The run's length in days.
    pub days: u32,
}

impl ObservedDimension {
    /// A dimension that moved `direction` for `days` days from `since`.
    pub fn new(
        dimension: impl Into<String>,
        direction: Direction,
        since: NaiveDate,
        days: u32,
    ) -> Self {
        Self { dimension: dimension.into(), direction, since, days }
    }
}

/// Who worded an observation's question (spec 3.2 §7.10): the model phrases only what the
/// deterministic detection found, and the kernel's template words it when the model is
/// unavailable, over budget, or answers with anything but a neutral question.
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::Phrasing;
///
/// let json = serde_json::to_value(Phrasing::Model { model: "gemini-3.5-flash-lite".into() }).unwrap();
/// assert_eq!(json["model"]["model"], "gemini-3.5-flash-lite");
/// assert_eq!(serde_json::to_value(Phrasing::Template).unwrap(), "template");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum Phrasing {
    /// The kernel's template ([`template_question`]).
    Template,
    /// A model.
    Model {
        /// The model's name.
        model: String,
    },
}

/// A change in a partition's routine the kernel noticed, offered as a question (spec 3.2
/// §7.10, K-I20).
///
/// It is recorded only after the warm-up, when the deviating dimensions are meaningful
/// (two dimensions, or one for ten days) and out of their seven-day cooldown, and its
/// [`question`](Self::question) passes [`is_neutral_question`].
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::Observation;
/// use chrono::NaiveDate;
///
/// let day = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
/// let id = Observation::id_for(&"work".into(), day);
/// assert!(id.starts_with("obs-20261022-"));
/// assert_eq!(id, Observation::id_for(&"work".into(), day));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Observation {
    /// `obs-<yyyymmdd>-<partition digest>`: one per partition and day, so replicas that
    /// both record it write one.
    pub id: String,
    /// The partition.
    pub partition: PartitionId,
    /// The local day the deviation test ran on.
    pub as_of: NaiveDate,
    /// The dimensions that changed, sorted by name.
    pub dimensions: Vec<ObservedDimension>,
    /// The comparison windows, in days.
    pub windows: Vec<u32>,
    /// The question offered to the person.
    pub question: String,
    /// Who worded it.
    pub phrasing: Phrasing,
    /// When the kernel recorded it.
    pub created_at: DateTime<Utc>,
}

impl Observation {
    /// The id of the observation of `partition` on `as_of`.
    pub fn id_for(partition: &PartitionId, as_of: NaiveDate) -> String {
        let digest = sha2::Sha256::digest(partition.as_str().as_bytes());
        format!("obs-{}-{}", as_of.format("%Y%m%d"), hex::encode(&digest[..4]))
    }

    /// What the observation found, as a statement a person confirms (spec 3.2 §7.10): the
    /// day the earliest deviating run began and each dimension's direction against the
    /// 28-day baseline, built from the dimension names alone. A confirmed observation is
    /// remembered as this text.
    ///
    /// # Example
    ///
    /// ```rust
    /// use adk_kernel::proc::{Direction, Observation, ObservedDimension, Phrasing};
    /// use chrono::NaiveDate;
    ///
    /// let day = NaiveDate::from_ymd_opt(2026, 10, 22).unwrap();
    /// let since = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
    /// let observation = Observation {
    ///     id: Observation::id_for(&"default".into(), day),
    ///     partition: "default".into(),
    ///     as_of: day,
    ///     dimensions: vec![ObservedDimension::new("activity.last_minute", Direction::Higher, since, 10)],
    ///     windows: vec![7, 14],
    ///     question: "Your routine has changed over the last two weeks. Would you like to review what changed?".into(),
    ///     phrasing: Phrasing::Template,
    ///     created_at: chrono::Utc::now(),
    /// };
    /// assert_eq!(
    ///     observation.statement(),
    ///     "Since 2026-10-13, compared with the 28 days before: the day's last activity has been later."
    /// );
    /// ```
    pub fn statement(&self) -> String {
        let since =
            self.dimensions.iter().map(|dimension| dimension.since).min().unwrap_or(self.as_of);
        let clauses: Vec<String> = self
            .dimensions
            .iter()
            .map(|dimension| {
                let (subject, higher, lower) = wording(&dimension.dimension);
                let comparison = match dimension.direction {
                    Direction::Higher => higher,
                    Direction::Lower => lower,
                };
                format!("{subject} has been {comparison}")
            })
            .collect();
        let changed = match clauses.as_slice() {
            [] => "the routine has changed".to_string(),
            [one] => one.clone(),
            [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
        };
        format!("Since {since}, compared with the 28 days before: {changed}.")
    }
}

/// What a baseline pass carries for a partition from one day to the next (spec 3.2
/// §7.10): the last day it tested, the drift state (each deviating dimension's run and
/// each dimension's cooldown) and the baselines it last computed.
///
/// `drift` is `adk_eval::ledger::DriftState` and `baselines` the pass's baseline snapshot,
/// both as JSON, so the stores hold them without the `proc-baselines` feature.
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::PassState;
/// use chrono::{NaiveDate, Utc};
///
/// let state = PassState::new("default", NaiveDate::from_ymd_opt(2026, 10, 22).unwrap(), Utc::now());
/// assert!(state.drift.is_null());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PassState {
    /// The partition.
    pub partition: PartitionId,
    /// The last local day the pass tested.
    pub last_day: NaiveDate,
    /// The drift state.
    #[serde(default)]
    pub drift: Value,
    /// The baselines on `last_day`.
    #[serde(default)]
    pub baselines: Value,
    /// When the pass wrote it.
    pub updated_at: DateTime<Utc>,
}

impl PassState {
    /// The state of `partition` after testing `last_day`, with no drift or baselines yet.
    pub fn new(
        partition: impl Into<PartitionId>,
        last_day: NaiveDate,
        updated_at: DateTime<Utc>,
    ) -> Self {
        Self {
            partition: partition.into(),
            last_day,
            drift: Value::Null,
            baselines: Value::Null,
            updated_at,
        }
    }
}

/// Words that diagnose, judge or instruct; an observation never carries one (spec 3.2
/// §7.10: "never a diagnosis").
const NOT_NEUTRAL: &[&str] = &[
    "diagnos",
    "burnout",
    "burn out",
    "burning out",
    "burned out",
    "burnt out",
    "depress",
    "anxi",
    "stress",
    "exhaust",
    "fatigue",
    "tired",
    "unhealthy",
    "health",
    "sick",
    "illness",
    "disorder",
    "addict",
    "obsess",
    "lazy",
    "overwork",
    "worr",
    "concern",
    "alarm",
    "problem",
    "wrong",
    "bad ",
    "should",
    "must",
    "need to",
    "you are",
    "you're",
    "you have",
    "symptom",
];

/// Whether `text` is a question in neutral language (spec 3.2 §7.10, K-I20): one line of
/// 20 to 400 characters ending in `?`, with no diagnosing, judging or instructing word.
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::is_neutral_question;
///
/// assert!(is_neutral_question(
///     "Your routine has changed over the last two weeks. Would you like to review what changed?"
/// ));
/// assert!(!is_neutral_question("Your routine has changed over the last two weeks."));
/// assert!(!is_neutral_question("Are you burning out from working late every night?"));
/// assert!(!is_neutral_question("You should stop working so late, don't you think?"));
/// ```
pub fn is_neutral_question(text: &str) -> bool {
    let text = text.trim();
    let lower = text.to_lowercase();
    (20..=400).contains(&text.chars().count())
        && text.ends_with('?')
        && !text.contains(['\n', '\r'])
        && !NOT_NEUTRAL.iter().any(|word| lower.contains(word))
}

/// How a dimension reads in a question: the subject, and the comparison for each
/// direction.
fn wording(dimension: &str) -> (String, &'static str, &'static str) {
    match dimension {
        "activity.last_minute" => ("the day's last activity".into(), "later", "earlier"),
        "activity.first_minute" => ("the day's first activity".into(), "later", "earlier"),
        _ => {
            let (kind, measure) = dimension.split_once('.').unwrap_or((dimension, "count"));
            let kind = match kind {
                "intent" => "requests".to_string(),
                "tool_call" => "tool calls".to_string(),
                "approval" => "approvals".to_string(),
                "rejection" => "rejections".to_string(),
                "denial" => "denied calls".to_string(),
                "held" => "held calls".to_string(),
                "turn" => "turns".to_string(),
                "wake" => "wake-ups".to_string(),
                "proposal" => "proposals".to_string(),
                "process" => "process changes".to_string(),
                "spend" => "spend".to_string(),
                other => other.replace('_', " "),
            };
            match measure {
                "count" => (format!("the number of {kind}"), "higher", "lower"),
                "duration_ms" => (format!("how long {kind} take"), "longer", "shorter"),
                other => {
                    let key = other.strip_prefix("meta.").unwrap_or(other).replace('_', " ");
                    (format!("{kind} {key}"), "higher", "lower")
                }
            }
        }
    }
}

/// The kernel's own wording of an observation (spec 3.2 §7.10): a neutral question built
/// from the dimension names and directions alone.
///
/// # Example
///
/// ```rust
/// use adk_kernel::proc::{Direction, ObservedDimension, is_neutral_question, template_question};
/// use chrono::NaiveDate;
///
/// let since = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
/// let question = template_question(
///     &[ObservedDimension::new("activity.last_minute", Direction::Higher, since, 10)],
///     &[7, 14],
/// );
/// assert_eq!(
///     question,
///     "Your routine has changed over the last two weeks: the day's last activity has been \
///      later than usual. Would you like to review what changed?"
/// );
/// assert!(is_neutral_question(&question));
/// ```
pub fn template_question(dimensions: &[ObservedDimension], windows: &[u32]) -> String {
    let period = match windows.iter().copied().max().unwrap_or(14) {
        0..=7 => "week".to_string(),
        days if days % 7 == 0 => {
            let weeks = days / 7;
            let words = ["", "one", "two", "three", "four"];
            match words.get(weeks as usize) {
                Some(word) => format!("{word} weeks"),
                None => format!("{weeks} weeks"),
            }
        }
        days => format!("{days} days"),
    };
    let clauses: Vec<String> = dimensions
        .iter()
        .map(|dimension| {
            let (subject, higher, lower) = wording(&dimension.dimension);
            let comparison = match dimension.direction {
                Direction::Higher => higher,
                Direction::Lower => lower,
            };
            format!("{subject} has been {comparison} than usual")
        })
        .collect();
    let changed = match clauses.as_slice() {
        [] => String::new(),
        [one] => format!(": {one}"),
        [rest @ .., last] => format!(": {}, and {last}", rest.join(", ")),
    };
    format!(
        "Your routine has changed over the last {period}{changed}. Would you like to review \
         what changed?"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
    }

    #[test]
    fn the_template_is_a_neutral_question_for_every_dimension_shape() {
        let dimensions = [
            ObservedDimension::new("activity.first_minute", Direction::Lower, day(1), 3),
            ObservedDimension::new("tool_call.count", Direction::Higher, day(1), 3),
            ObservedDimension::new("tool_call.duration_ms", Direction::Lower, day(1), 3),
            ObservedDimension::new("turn.meta.cost_micro_usd", Direction::Higher, day(1), 3),
            ObservedDimension::new("review_item.count", Direction::Lower, day(1), 3),
        ];
        for windows in [[7, 14], [7, 7], [14, 28], [5, 10]] {
            for take in 1..=dimensions.len() {
                let question = template_question(&dimensions[..take], &windows);
                assert!(is_neutral_question(&question), "{question}");
            }
        }
        let question = template_question(&dimensions[..2], &[7, 14]);
        assert_eq!(
            question,
            "Your routine has changed over the last two weeks: the day's first activity has \
             been earlier than usual, and the number of tool calls has been higher than usual. \
             Would you like to review what changed?"
        );
    }

    #[test]
    fn a_diagnosis_a_statement_or_an_instruction_is_not_neutral() {
        for text in [
            "You have been working late; this looks like burnout?",
            "Is something wrong with your schedule lately?",
            "Your routine changed over the last two weeks.",
            "Review it?",
            "Your evenings look stressful lately — want to talk about it?",
            "Two lines\nwould you like to review what changed?",
        ] {
            assert!(!is_neutral_question(text), "{text}");
        }
    }

    #[test]
    fn ids_are_per_partition_and_day() {
        assert_ne!(
            Observation::id_for(&"work".into(), day(1)),
            Observation::id_for(&"home".into(), day(1))
        );
        assert_ne!(
            Observation::id_for(&"work".into(), day(1)),
            Observation::id_for(&"work".into(), day(2))
        );
    }
}
