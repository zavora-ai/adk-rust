//! The first catalogue (owner question 21): `coming_up`, the caller's Coming up with the two
//! times each item may move to, and `snooze`, which moves one an hour later or to the same time
//! tomorrow (spec 3.3 §2 item 7, §5.1).

use std::sync::Arc;

use adk_kernel::drivers::EffectClass;
use adk_kernel::{AgendaItem, AgendaState};
use async_trait::async_trait;
use chrono::{DateTime, Duration, FixedOffset, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::registry::{Entry, single_time};
use super::{Callers, ObjectKind, VerbDone, VerbError, VerbHandler, VerbScope, VerbSpec, words};

/// Something in the caller's Coming up, and the times it may move to (spec 3.3 §5.1).
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::ComingUpItem;
///
/// let item: ComingUpItem = serde_json::from_value(serde_json::json!({
///     "id": "agenda-7f3", "title": "News digest", "area": "work",
///     "at": "2026-10-26T15:00:00Z", "atText": "18:00", "choices": [],
/// }))
/// .unwrap();
/// assert_eq!(item.title, "News digest");
/// assert!(item.why_not.is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComingUpItem {
    /// The agenda item's id.
    pub id: String,
    /// Its title.
    pub title: String,
    /// The area it is in.
    pub area: String,
    /// When it wakes, when it waits for one time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<DateTime<Utc>>,
    /// When it wakes, as a person reads it: "18:00", "Tue 18:00"; empty without one time.
    pub at_text: String,
    /// When it ends if it has not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends: Option<DateTime<Utc>>,
    /// The times it may move to: an hour later, the same time tomorrow — those before it ends.
    pub choices: Vec<SnoozeChoice>,
    /// Why it cannot be snoozed, in plain words; absent when it can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why_not: Option<String>,
}

/// One time an item may move to (spec 3.3 §5.1).
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::SnoozeChoice;
///
/// let choice: SnoozeChoice = serde_json::from_value(serde_json::json!({
///     "to": "2026-10-26T19:00:00+03:00", "label": "1 hour later", "atText": "19:00", "tomorrow": false,
/// }))
/// .unwrap();
/// assert_eq!(choice.to.to_rfc3339(), "2026-10-26T19:00:00+03:00");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SnoozeChoice {
    /// The new time, the `to` of `snooze`.
    pub to: DateTime<FixedOffset>,
    /// "1 hour later" or "Tomorrow, same time".
    pub label: String,
    /// The new time as a person reads it: "19:00", "Tue 18:00".
    pub at_text: String,
    /// It is the same time tomorrow.
    pub tomorrow: bool,
}

/// `coming_up` and `snooze`.
pub(crate) fn catalogue() -> Vec<Entry> {
    let coming_up = VerbSpec::new(
        "coming_up",
        "Coming up",
        "List what is coming up for the person: each item's id, title, when it wakes, and the \
         two times it may be snoozed to. Reading it never asks the person.",
        EffectClass::Read,
    )
    .with_objects([ObjectKind::ComingUp])
    .with_callers(Callers::PersonAndAgents);
    let snooze = VerbSpec::new(
        "snooze",
        words::SNOOZE,
        words::SNOOZE_SOMETHING,
        EffectClass::WriteLocal,
    )
    .with_objects([ObjectKind::ComingUp])
    .with_args(json!({
        "type": "object",
        "properties": {
            "item": {
                "type": "string",
                "description": "The id of the item, as coming_up lists it"
            },
            "to": {
                "type": "string",
                "format": "date-time",
                "description": "When it should wake instead: exactly one of the two times coming_up lists for the item"
            }
        },
        "required": ["item", "to"],
        "additionalProperties": false
    }))
    .with_callers(Callers::PersonAndAgents)
    .only_delays()
    .reversible()
    .with_voice_phrase("snooze");
    vec![
        Entry { spec: coming_up, handler: Arc::new(ComingUp) },
        Entry { spec: snooze, handler: Arc::new(Snooze) },
    ]
}

impl VerbScope {
    /// The caller's Coming up: every scheduled or running item, soonest first, with the times
    /// each may move to and why one cannot.
    ///
    /// # Errors
    ///
    /// The kernel's or the store's error.
    pub async fn coming_up(&self) -> Result<Vec<ComingUpItem>, VerbError> {
        let mut out = Vec::new();
        for item in self.items().await? {
            out.push(self.coming_up_item(&item).await?);
        }
        out.sort_by(|a, b| match (a.at, b.at) {
            (Some(a), Some(b)) => a.cmp(&b),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });
        Ok(out)
    }

    async fn coming_up_item(&self, item: &AgendaItem) -> Result<ComingUpItem, VerbError> {
        let at = match &item.state {
            AgendaState::Scheduled { wake } => single_time(wake),
            _ => None,
        };
        let why_not = match &item.state {
            AgendaState::Running { .. } => Some(words::ALREADY_STARTED),
            AgendaState::Scheduled { .. } if at.is_none() => Some(words::NOT_ONE_TIME),
            AgendaState::Scheduled { .. } if self.started(item).await? => {
                Some(words::ALREADY_STARTED)
            }
            AgendaState::Scheduled { .. } => None,
            _ => Some(words::NOT_SCHEDULED),
        };
        let choices = match (at, why_not) {
            (Some(at), None) => self.choices(at, item.expires_at),
            _ => Vec::new(),
        };
        let why_not = why_not.map(str::to_string).or_else(|| match (at, item.expires_at) {
            (Some(_), Some(ends)) if choices.is_empty() => {
                Some(words::ends_at(&self.time_text(ends)))
            }
            _ => None,
        });
        Ok(ComingUpItem {
            id: item.id.clone(),
            title: item.title.clone(),
            area: item.partition.to_string(),
            at,
            at_text: at.map(|at| self.time_text(at)).unwrap_or_default(),
            ends: item.expires_at,
            choices,
            why_not,
        })
    }

    /// The times `at` may move to before `ends`.
    fn choices(&self, at: DateTime<Utc>, ends: Option<DateTime<Utc>>) -> Vec<SnoozeChoice> {
        [
            (Duration::hours(1), words::ONE_HOUR_LATER, false),
            (Duration::days(1), words::TOMORROW_SAME_TIME, true),
        ]
        .into_iter()
        .map(|(by, label, tomorrow)| (at + by, label, tomorrow))
        .filter(|(to, _, _)| ends.is_none_or(|ends| *to <= ends))
        .map(|(to, label, tomorrow)| SnoozeChoice {
            to: to.with_timezone(&self.offset()),
            label: label.to_string(),
            at_text: self.time_text(to),
            tomorrow,
        })
        .collect()
    }

    /// The item `id` and the wake it moves from, when moving it to `to` is one of the times it
    /// may move to now.
    async fn snoozable(
        &self,
        id: &str,
        to: DateTime<Utc>,
    ) -> Result<(AgendaItem, DateTime<Utc>), VerbError> {
        let item = self.item(id).await?;
        let wake = match &item.state {
            AgendaState::Scheduled { wake } => wake,
            AgendaState::Running { .. } => return Err(VerbError::refused(words::ALREADY_STARTED)),
            AgendaState::Proposed | AgendaState::Approved => {
                return Err(VerbError::refused(words::NOT_SCHEDULED));
            }
            _ => return Err(VerbError::refused(words::ENDED)),
        };
        let Some(at) = single_time(wake) else {
            return Err(VerbError::refused(words::NOT_ONE_TIME));
        };
        if to != at + Duration::hours(1) && to != at + Duration::days(1) {
            // Already as late as asked: someone moved it since. Any other time is not one of
            // its two, as an Edit can give it.
            if to <= at {
                return Err(VerbError::refused(words::IT_MOVED));
            }
            let times: Vec<String> = self
                .choices(at, item.expires_at)
                .into_iter()
                .map(|choice| choice.at_text)
                .collect();
            return Err(VerbError::refused(match (times.is_empty(), item.expires_at) {
                (true, Some(ends)) => words::ends_at(&self.time_text(ends)),
                _ => words::only_moves_to(&times),
            }));
        }
        if let Some(ends) = item.expires_at.filter(|ends| to > *ends) {
            return Err(VerbError::refused(words::ends_at(&self.time_text(ends))));
        }
        if self.started(&item).await? {
            return Err(VerbError::refused(words::ALREADY_STARTED));
        }
        Ok((item, at))
    }
}

/// The item whose title is `title`, ignoring case, among `items` (spec 3.3 §5.2): one exact
/// match. Without one, a title that is part of several is ambiguous and lists them.
///
/// # Errors
///
/// [`VerbError::NotFound`] when no title is `title`; [`VerbError::Ambiguous`] when several
/// are, or when none is and several contain it.
///
/// # Example
///
/// ```rust
/// use adk_surface::verbs::{ComingUpItem, find_by_title};
///
/// let item = |id: &str, title: &str| ComingUpItem {
///     id: id.into(), title: title.into(), area: "work".into(), at: None,
///     at_text: String::new(), ends: None, choices: vec![], why_not: None,
/// };
/// let items = [item("a", "News digest"), item("b", "Weekly digest")];
/// assert_eq!(find_by_title(&items, "news DIGEST").unwrap().id, "a");
/// assert_eq!(find_by_title(&items, "digest").unwrap_err().code(), "surface.verb.ambiguous");
/// assert_eq!(find_by_title(&items, "laundry").unwrap_err().code(), "surface.verb.unknown");
/// ```
pub fn find_by_title<'a>(
    items: &'a [ComingUpItem],
    title: &str,
) -> Result<&'a ComingUpItem, VerbError> {
    let wanted = normalize(title);
    let exact: Vec<&ComingUpItem> =
        items.iter().filter(|item| normalize(&item.title) == wanted).collect();
    match exact.as_slice() {
        [only] => return Ok(only),
        [] => {}
        several => {
            return Err(VerbError::Ambiguous {
                said: title.to_string(),
                matches: several.iter().map(|item| item.title.clone()).collect(),
            });
        }
    }
    let partial: Vec<&ComingUpItem> = items
        .iter()
        .filter(|item| !wanted.is_empty() && normalize(&item.title).contains(&wanted))
        .collect();
    if partial.len() > 1 {
        return Err(VerbError::Ambiguous {
            said: title.to_string(),
            matches: partial.iter().map(|item| item.title.clone()).collect(),
        });
    }
    Err(VerbError::not_found(format!("“{title}”")))
}

/// `text` lower-cased with its words single-spaced, punctuation dropped.
pub(crate) fn normalize(text: &str) -> String {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The item and the new time a `snooze` call names.
fn snooze_args(args: &Value) -> Result<(String, DateTime<Utc>), VerbError> {
    let item = args.get("item").and_then(Value::as_str).filter(|item| !item.is_empty());
    let to = args.get("to").and_then(Value::as_str);
    let (Some(item), Some(to)) = (item, to) else {
        return Err(VerbError::invalid(
            "snooze takes `item`, the id coming_up lists, and `to`, one of its two times",
        ));
    };
    let to = DateTime::parse_from_rfc3339(to)
        .map_err(|_| VerbError::invalid(format!("`to` is not a time with its offset: {to}")))?
        .with_timezone(&Utc);
    Ok((item.to_string(), to))
}

/// `coming_up`: the caller's Coming up, as data.
struct ComingUp;

#[async_trait]
impl VerbHandler for ComingUp {
    async fn describe(&self, _scope: &VerbScope, _args: &Value) -> Result<String, VerbError> {
        Ok("List what is coming up".to_string())
    }

    async fn run(&self, scope: &VerbScope, _args: &Value) -> Result<VerbDone, VerbError> {
        let items = scope.coming_up().await?;
        let text = match items.len() {
            0 => "Nothing is coming up".to_string(),
            1 => "1 thing is coming up".to_string(),
            count => format!("{count} things are coming up"),
        };
        let result = serde_json::to_value(&items).unwrap_or(Value::Null);
        Ok(VerbDone::new("coming_up", text).with_result(json!({ "items": result })))
    }
}

/// `snooze`: moves one item an hour later or to the same time tomorrow.
struct Snooze;

#[async_trait]
impl VerbHandler for Snooze {
    async fn describe(&self, scope: &VerbScope, args: &Value) -> Result<String, VerbError> {
        let (id, to) = snooze_args(args)?;
        let (item, _) = scope.snoozable(&id, to).await?;
        Ok(words::move_to(&item.title, &scope.time_text(to)))
    }

    async fn run(&self, scope: &VerbScope, args: &Value) -> Result<VerbDone, VerbError> {
        let (id, to) = snooze_args(args)?;
        let (item, from) = scope.snoozable(&id, to).await?;
        scope.move_wake(&id, from, to).await?;
        let at_text = scope.time_text(to);
        Ok(VerbDone::new("snooze", words::moved_to(&item.title, &at_text))
            .with_target(format!("agenda:{id}"), item.title.clone())
            .with_result(json!({
                "item": id,
                "title": item.title,
                "from": from,
                "to": to.with_timezone(&scope.offset()),
                "atText": at_text,
            })))
    }
}
