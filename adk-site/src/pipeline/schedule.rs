//! Schedules as data (spec 3.5 §7.8.2 "Schedules"): the times at which an item publishes,
//! leaves its embargo, expires or reverts, so a kernel wake can be set for the next one.

use awp_types::{ContentId, ContentItem, ContentState};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What happens to an item at a scheduled time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledActionKind {
    /// A scheduled item publishes (`publish_at`).
    Publish,
    /// An embargo ends (`embargo_until`).
    LiftEmbargo,
    /// The item expires and leaves the site, or is marked expired (`expires_at`).
    Expire,
    /// The item returns to its previous released version (`revert_at`).
    Revert,
}

/// One scheduled action.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledAction {
    /// When.
    pub at: DateTime<Utc>,
    /// Which item.
    pub item: ContentId,
    /// What.
    pub kind: ScheduledActionKind,
}

/// Every scheduled action of `items`, earliest first.
///
/// # Example
///
/// ```rust
/// use adk_site::pipeline::{ScheduledActionKind, schedule_actions};
/// # use awp_types::{ContentItem, ContentKind, ContentState, Schedule};
/// # use chrono::{TimeZone, Utc};
/// # let at = Utc.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap();
/// # let item = ContentItem { id: "products/p/en".into(), kind: ContentKind::Product, slug: "p".into(),
/// #     language: "en".parse().unwrap(), fields: Default::default(), volatile: vec![], body: None, media: vec![],
/// #     sources: vec![], schedule: Some(Schedule { publish_at: Some(at), ..Schedule::default() }),
/// #     state: ContentState::Scheduled };
/// let actions = schedule_actions(&[item]);
/// assert_eq!(actions[0].kind, ScheduledActionKind::Publish);
/// ```
pub fn schedule_actions(items: &[ContentItem]) -> Vec<ScheduledAction> {
    let mut actions = Vec::new();
    for item in items {
        let Some(schedule) = &item.schedule else { continue };
        let live = matches!(
            item.state,
            ContentState::Published
                | ContentState::Corrected
                | ContentState::Scheduled
                | ContentState::Approved
        );
        let mut push = |at: Option<DateTime<Utc>>, kind| {
            if let Some(at) = at {
                actions.push(ScheduledAction { at, item: item.id.clone(), kind });
            }
        };
        if item.state == ContentState::Scheduled {
            push(schedule.publish_at, ScheduledActionKind::Publish);
        }
        if live {
            push(schedule.embargo_until, ScheduledActionKind::LiftEmbargo);
            push(schedule.expires_at, ScheduledActionKind::Expire);
            push(schedule.revert_at, ScheduledActionKind::Revert);
        }
    }
    actions.sort();
    actions
}

/// The actions due after `after` (exclusive) and up to `now` (inclusive).
pub fn due_actions(
    items: &[ContentItem],
    after: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Vec<ScheduledAction> {
    schedule_actions(items)
        .into_iter()
        .filter(|a| a.at <= now && after.is_none_or(|after| a.at > after))
        .collect()
}

/// The time of the next action after `now`: where the kernel sets its next wake.
pub fn next_wake(items: &[ContentItem], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    schedule_actions(items).into_iter().map(|a| a.at).find(|at| *at > now)
}
