//! [`WatchEvaluator`]: watch conditions held against tracked observations.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use super::{TrackedObservation, WatchCondition, WatchEvent, WatchEventKind};
use crate::error::{Result, VisionError};
use crate::geometry::{Region, TrackId};
use crate::media::FrameSourceId;

/// Holds [`WatchCondition`]s against each frame's tracked regions and returns the events that
/// pass (spec 3.7 §7.10). One evaluator serves any number of sources, each with its own state.
///
/// For each condition and track it counts the consecutive observations in which the track
/// has a region meeting the condition; a frame without one starts the count again. A track
/// fires once its count reaches the condition's `min_frames`, once per condition and session.
/// A condition with a cooldown fires at most once per cooldown, by capture time: the best
/// track then (highest score, then lowest identifier) fires, and the others that qualify wait
/// for the cooldown to pass and fire then if they still qualify. Without a cooldown every
/// qualifying track fires.
///
/// An observation of a new session forgets the source's state. An observation whose frame
/// index is not past the last one seen for the session is ignored, so a redelivery or a late
/// frame changes nothing. A track the observation does not list as held is forgotten.
///
/// The `watch` module documentation has an example.
#[derive(Clone, Debug)]
pub struct WatchEvaluator {
    conditions: Vec<WatchCondition>,
    sources: HashMap<FrameSourceId, SourceState>,
}

/// One source's state within its session.
#[derive(Clone, Debug)]
struct SourceState {
    session: String,
    last_index: Option<u64>,
    /// Consecutive observations each (condition, track) met its condition in.
    streaks: HashMap<(usize, TrackId), u32>,
    /// The (condition, track) pairs that fired.
    fired: HashSet<(usize, TrackId)>,
    /// When each condition last fired.
    last_fire: Vec<Option<DateTime<Utc>>>,
}

impl SourceState {
    fn new(session: &str, conditions: usize) -> Self {
        Self {
            session: session.to_string(),
            last_index: None,
            streaks: HashMap::new(),
            fired: HashSet::new(),
            last_fire: vec![None; conditions],
        }
    }
}

impl WatchEvaluator {
    /// An evaluator for `conditions`.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for an invalid condition or two conditions with one name.
    pub fn new(conditions: Vec<WatchCondition>) -> Result<Self> {
        let mut names = HashSet::new();
        for condition in &conditions {
            condition.validate()?;
            if !names.insert(condition.name.as_str()) {
                return Err(VisionError::InvalidInput(format!(
                    "two watch conditions are named `{}`; each name keys its own events",
                    condition.name
                )));
            }
        }
        Ok(Self { conditions, sources: HashMap::new() })
    }

    /// The conditions, in the order they were given.
    pub fn conditions(&self) -> &[WatchCondition] {
        &self.conditions
    }

    /// The condition named `name`.
    pub fn condition(&self, name: &str) -> Option<&WatchCondition> {
        self.conditions.iter().find(|condition| condition.name == name)
    }

    /// Adds a condition while the stream runs. It starts with no streaks; the other
    /// conditions keep theirs.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for an invalid condition or a name already in use.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::{WatchCondition, WatchEvaluator};
    ///
    /// let mut watch = WatchEvaluator::new(Vec::new()).unwrap();
    /// watch.add(WatchCondition::new("seal", ["torn seal"])).unwrap();
    /// assert!(watch.add(WatchCondition::new("seal", ["scratch"])).is_err());
    /// assert_eq!(watch.remove("seal").unwrap().labels, vec!["torn seal".to_string()]);
    /// assert!(watch.conditions().is_empty());
    /// ```
    pub fn add(&mut self, condition: WatchCondition) -> Result<()> {
        condition.validate()?;
        if self.condition(&condition.name).is_some() {
            return Err(VisionError::InvalidInput(format!(
                "a watch condition named `{}` is already registered; each name keys its own events",
                condition.name
            )));
        }
        self.conditions.push(condition);
        for state in self.sources.values_mut() {
            state.last_fire.push(None);
        }
        Ok(())
    }

    /// Removes the condition named `name`, with its state; the other conditions keep theirs.
    pub fn remove(&mut self, name: &str) -> Option<WatchCondition> {
        let index = self.conditions.iter().position(|condition| condition.name == name)?;
        let removed = self.conditions.remove(index);
        let shift = |c: usize| if c > index { c - 1 } else { c };
        for state in self.sources.values_mut() {
            state.last_fire.remove(index);
            state.streaks = std::mem::take(&mut state.streaks)
                .into_iter()
                .filter(|((c, _), _)| *c != index)
                .map(|((c, track), streak)| ((shift(c), track), streak))
                .collect();
            state.fired = std::mem::take(&mut state.fired)
                .into_iter()
                .filter(|(c, _)| *c != index)
                .map(|(c, track)| (shift(c), track))
                .collect();
        }
        Some(removed)
    }

    /// Forgets every source's state, as a new session would.
    pub fn reset(&mut self) {
        self.sources.clear();
    }

    /// Holds every condition against one frame and returns the events that passed, in
    /// condition order.
    pub fn observe(&mut self, observation: &TrackedObservation) -> Vec<WatchEvent> {
        let conditions = self.conditions.len();
        let state = self
            .sources
            .entry(observation.source.clone())
            .or_insert_with(|| SourceState::new(&observation.session, conditions));
        if state.session != observation.session {
            *state = SourceState::new(&observation.session, conditions);
        }
        if state.last_index.is_some_and(|last| observation.frame.index <= last) {
            tracing::debug!(
                vision.source = %observation.source.0,
                vision.frame = observation.frame.index,
                "observation not past the last frame seen; ignored"
            );
            return Vec::new();
        }
        state.last_index = Some(observation.frame.index);

        // A track seen in the frame is held whether or not the list names it.
        let held: HashSet<TrackId> = observation
            .tracks
            .iter()
            .copied()
            .chain(observation.regions.iter().filter_map(|r| r.track))
            .collect();
        state.fired.retain(|(_, track)| held.contains(track));

        let mut events = Vec::new();
        for (index, condition) in self.conditions.iter().enumerate() {
            // Each track's best region meeting the condition in this frame.
            let mut best: HashMap<TrackId, &Region> = HashMap::new();
            for region in observation.regions.iter().filter(|r| condition.matches(r)) {
                let Some(track) = region.track else { continue };
                let better = best.get(&track).is_none_or(|kept| score(region) > score(kept));
                if better {
                    best.insert(track, region);
                }
            }
            state.streaks.retain(|(c, track), _| *c != index || best.contains_key(track));
            let mut ready: Vec<(TrackId, &Region, u32)> = Vec::new();
            for (&track, &region) in &best {
                let streak = state.streaks.entry((index, track)).or_insert(0);
                *streak = streak.saturating_add(1);
                if *streak >= condition.min_frames && !state.fired.contains(&(index, track)) {
                    ready.push((track, region, *streak));
                }
            }
            if ready.is_empty() {
                continue;
            }
            let captured = observation.frame.captured_at;
            let cooling = state.last_fire[index].is_some_and(|last| {
                captured.signed_duration_since(last).to_std().unwrap_or_default()
                    < condition.cooldown
            });
            if cooling {
                continue;
            }
            ready.sort_by(|a, b| score(b.1).total_cmp(&score(a.1)).then(a.0.cmp(&b.0)));
            if !condition.cooldown.is_zero() {
                ready.truncate(1);
            }
            for (track, region, frames) in ready {
                state.fired.insert((index, track));
                state.last_fire[index] = Some(captured);
                events.push(WatchEvent {
                    source: observation.source.clone(),
                    session: observation.session.clone(),
                    condition: condition.name.clone(),
                    kind: WatchEventKind::Detected,
                    track,
                    label: region.label.clone().unwrap_or_default(),
                    score: score(region),
                    frames,
                    frame: observation.frame.clone(),
                    regions: event_regions(condition, observation, region),
                });
            }
        }
        events
    }
}

fn score(region: &Region) -> f32 {
    region.score.unwrap_or(0.0)
}

/// The firing region first, then the frame's other regions meeting the condition by score.
fn event_regions(
    condition: &WatchCondition,
    observation: &TrackedObservation,
    firing: &Region,
) -> Vec<Region> {
    let mut others: Vec<&Region> = observation
        .regions
        .iter()
        .filter(|r| !std::ptr::eq(*r, firing) && condition.matches(r))
        .collect();
    others.sort_by(|a, b| score(b).total_cmp(&score(a)));
    std::iter::once(firing).chain(others).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BackendId;
    use crate::digest::Digest;
    use crate::geometry::{BBox, CoordFrame, RegionOrigin, Shape, TransformChain};
    use crate::watch::FrameRef;

    fn seen(index: u64, labels: &[(&str, u64)]) -> TrackedObservation {
        let digest = Digest::of(&index.to_be_bytes());
        let regions = labels
            .iter()
            .map(|(label, track)| Region {
                image: digest,
                shape: Shape::Box(BBox::new(10.0, 10.0, 40.0, 40.0)),
                label: Some((*label).into()),
                score: Some(0.9),
                track: Some(TrackId(*track)),
                origin: RegionOrigin {
                    backend: BackendId::new("scripted"),
                    manifest: None,
                    frame: CoordFrame::SentPixels,
                    chain: TransformChain::new(640, 480),
                    escalated_from: None,
                },
            })
            .collect();
        TrackedObservation::new(
            FrameSourceId("cam".into()),
            "s1",
            FrameRef::new(index, Utc::now(), digest, 640, 480),
            regions,
        )
    }

    #[test]
    fn adding_and_removing_a_condition_keeps_the_others_streaks() {
        let mut watch = WatchEvaluator::new(vec![
            WatchCondition::new("first", ["seal"]).with_min_frames(1),
            WatchCondition::new("second", ["seal"]).with_min_frames(3),
        ])
        .unwrap();
        // `first` fires at once; `second` has a streak of two.
        let fired: Vec<String> = (0..2)
            .flat_map(|i| watch.observe(&seen(i, &[("seal", 4)])))
            .map(|e| e.condition)
            .collect();
        assert_eq!(fired, vec!["first"]);
        // Removing `first` shifts `second` to index 0 with its streak intact.
        assert!(watch.remove("first").is_some());
        assert!(watch.remove("first").is_none());
        watch.add(WatchCondition::new("third", ["seal"]).with_min_frames(2)).unwrap();
        let next: Vec<String> =
            watch.observe(&seen(2, &[("seal", 4)])).into_iter().map(|e| e.condition).collect();
        assert_eq!(next, vec!["second"], "the third frame of `second`'s streak; `third` has one");
        let after: Vec<String> =
            watch.observe(&seen(3, &[("seal", 4)])).into_iter().map(|e| e.condition).collect();
        assert_eq!(after, vec!["third"]);
        // `second` and `third` fired for track 4 once each; nothing fires again.
        assert!(watch.observe(&seen(4, &[("seal", 4)])).is_empty());
    }
}
