//! [`WatchCondition`] and its [`Zone`].

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::digest::Digest;
use crate::error::{Result, VisionError};
use crate::geometry::{Polygon, Region};

/// The longest condition name, in characters.
const MAX_NAME_CHARS: usize = 64;

/// Which point of a region must lie in a [`Zone`].
///
/// # Example
///
/// ```
/// use adk_vision::ZoneAnchor;
///
/// assert_eq!(ZoneAnchor::default(), ZoneAnchor::Center);
/// assert_eq!(serde_json::to_value(ZoneAnchor::BottomCenter).unwrap(), "bottomCenter");
/// ```
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ZoneAnchor {
    /// The centre of the region's box.
    #[default]
    Center,
    /// The middle of the box's bottom edge: where an object meets the floor or the belt.
    BottomCenter,
}

/// An area of the frame a [`WatchCondition`] is limited to: a polygon in source pixels and
/// the point of each region that must lie inside it.
///
/// Containment is even-odd ray casting with half-open edges, so a zone drawn as an
/// axis-aligned rectangle holds exactly the points [`BBox::contains`](crate::BBox::contains)
/// holds.
///
/// # Example
///
/// ```
/// use adk_vision::{Zone, ZoneAnchor};
///
/// // The left half of a 1280x720 frame.
/// let zone = Zone::new(vec![[0.0, 0.0], [640.0, 0.0], [640.0, 720.0], [0.0, 720.0]]).unwrap();
/// assert!(zone.contains([100.0, 300.0]));
/// assert!(!zone.contains([700.0, 300.0]));
/// assert_eq!(zone.with_anchor(ZoneAnchor::BottomCenter).anchor, ZoneAnchor::BottomCenter);
/// assert!(Zone::new(vec![[0.0, 0.0], [1.0, 1.0]]).is_err());
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Zone {
    /// The polygon, vertices in order, in source pixels.
    pub polygon: Polygon,
    /// The point of each region that must lie inside.
    #[serde(default)]
    pub anchor: ZoneAnchor,
}

impl Zone {
    /// A zone over the polygon with these vertices, anchored at each region's centre.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for fewer than three vertices or a coordinate that is
    /// not finite.
    pub fn new(points: Vec<[f32; 2]>) -> Result<Self> {
        let zone = Self { polygon: Polygon { points }, anchor: ZoneAnchor::default() };
        zone.validate()?;
        Ok(zone)
    }

    /// Sets the point of each region that must lie inside.
    #[must_use]
    pub fn with_anchor(mut self, anchor: ZoneAnchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// Checks the polygon: three vertices or more, every coordinate finite.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] naming what is wrong.
    pub fn validate(&self) -> Result<()> {
        let points = &self.polygon.points;
        if points.len() < 3 {
            return Err(VisionError::InvalidInput(format!(
                "a zone is a polygon of three vertices or more, not {}",
                points.len()
            )));
        }
        if points.iter().flatten().any(|c| !c.is_finite()) {
            return Err(VisionError::InvalidInput(
                "a zone's vertices must be finite pixel coordinates".into(),
            ));
        }
        Ok(())
    }

    /// Whether `point` lies inside the polygon (even-odd rule, half-open edges).
    pub fn contains(&self, [x, y]: [f32; 2]) -> bool {
        let points = &self.polygon.points;
        let mut inside = false;
        let mut previous = match points.last() {
            Some(last) => *last,
            None => return false,
        };
        for &current in points {
            let ([xi, yi], [xj, yj]) = (current, previous);
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            previous = current;
        }
        inside
    }

    /// Whether the region's anchor point lies inside; a region with no extent is outside.
    pub fn holds(&self, region: &Region) -> bool {
        let Some(bbox) = region.shape.bbox() else { return false };
        let point = match self.anchor {
            ZoneAnchor::Center => bbox.center(),
            ZoneAnchor::BottomCenter => [(bbox.x0 + bbox.x1) / 2.0, bbox.y1],
        };
        self.contains(point)
    }
}

/// What a stream watches for, evaluated on the device so the kernel sees only the events
/// that pass (spec 3.7 §7.10).
///
/// A region meets the condition when its label is one of `labels` (compared ignoring ASCII
/// case; an empty list takes any label), its score is at least `min_score` (a region with no
/// score has none), and, with a zone, its anchor point lies in the zone. A track fires once
/// it has met the condition in `min_frames` consecutive observations. After a firing, the
/// condition fires again no sooner than `cooldown` later, by the frames' capture times.
///
/// | Field | Default |
/// |-------|---------|
/// | `min_score` | `0.5` |
/// | `min_frames` | `3` |
/// | `zone` | none: the whole frame |
/// | `cooldown` | zero |
///
/// # Example
///
/// ```
/// use std::time::Duration;
/// use adk_vision::WatchCondition;
///
/// let seal = WatchCondition::new("torn-seal", ["torn seal"])
///     .with_min_score(0.6)
///     .with_min_frames(3)
///     .with_cooldown(Duration::from_secs(2));
/// assert!(seal.validate().is_ok());
/// let json = serde_json::to_value(&seal).unwrap();
/// assert_eq!(json["cooldownMs"], 2000);
/// assert!(WatchCondition::new("torn seal", ["seal"]).validate().is_err(), "names are keys");
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchCondition {
    /// The condition's name: part of every event key, so `[A-Za-z0-9._-]`, at most 64.
    pub name: String,
    /// The labels that count; empty for any.
    #[serde(default)]
    pub labels: Vec<String>,
    /// The lowest score that counts, in `[0, 1]`.
    pub min_score: f32,
    /// How many consecutive observations a track must meet the condition in, at least 1.
    pub min_frames: u32,
    /// The area of the frame the condition is limited to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<Zone>,
    /// The least time between two firings of the condition.
    #[serde(default, rename = "cooldownMs", with = "millis")]
    pub cooldown: Duration,
}

impl WatchCondition {
    /// A condition named `name` over `labels`, with the defaults in the table above.
    pub fn new<L: Into<String>>(
        name: impl Into<String>,
        labels: impl IntoIterator<Item = L>,
    ) -> Self {
        Self {
            name: name.into(),
            labels: labels.into_iter().map(Into::into).collect(),
            min_score: 0.5,
            min_frames: 3,
            zone: None,
            cooldown: Duration::ZERO,
        }
    }

    /// Sets the lowest score that counts.
    #[must_use]
    pub fn with_min_score(mut self, min_score: f32) -> Self {
        self.min_score = min_score;
        self
    }

    /// Sets how many consecutive observations a track must meet the condition in.
    #[must_use]
    pub fn with_min_frames(mut self, min_frames: u32) -> Self {
        self.min_frames = min_frames;
        self
    }

    /// Limits the condition to `zone`.
    #[must_use]
    pub fn with_zone(mut self, zone: Zone) -> Self {
        self.zone = Some(zone);
        self
    }

    /// Sets the least time between two firings.
    #[must_use]
    pub fn with_cooldown(mut self, cooldown: Duration) -> Self {
        self.cooldown = cooldown;
        self
    }

    /// Checks the condition.
    ///
    /// # Errors
    ///
    /// [`VisionError::InvalidInput`] for a name outside `[A-Za-z0-9._-]` or longer than 64
    /// characters, a `min_score` outside `[0, 1]`, a `min_frames` of zero, or an invalid
    /// zone.
    pub fn validate(&self) -> Result<()> {
        let name_ok = !self.name.is_empty()
            && self.name.chars().count() <= MAX_NAME_CHARS
            && self.name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !name_ok {
            return Err(VisionError::InvalidInput(format!(
                "watch condition name `{}` must be 1 to {MAX_NAME_CHARS} characters of \
                 [A-Za-z0-9._-]: it is part of every event key",
                self.name
            )));
        }
        if !(0.0..=1.0).contains(&self.min_score) {
            return Err(VisionError::InvalidInput(format!(
                "watch condition `{}` has min_score {}; use a score in [0, 1]",
                self.name, self.min_score
            )));
        }
        if self.min_frames == 0 {
            return Err(VisionError::InvalidInput(format!(
                "watch condition `{}` needs min_frames of at least 1",
                self.name
            )));
        }
        if let Some(zone) = &self.zone {
            zone.validate()?;
        }
        Ok(())
    }

    /// Whether `region` meets the condition in this one frame: label, score and zone.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_vision::{BBox, BackendId, CoordFrame, Digest, Region, RegionOrigin, Shape, TransformChain, WatchCondition};
    ///
    /// let region = Region {
    ///     image: Digest::of(b"frame"),
    ///     shape: Shape::Box(BBox::new(10.0, 10.0, 50.0, 30.0)),
    ///     label: Some("Torn Seal".into()),
    ///     score: Some(0.8),
    ///     track: None,
    ///     origin: RegionOrigin {
    ///         backend: BackendId::new("rf-detr"),
    ///         manifest: None,
    ///         frame: CoordFrame::SentPixels,
    ///         chain: TransformChain::new(640, 480),
    ///         escalated_from: None,
    ///     },
    /// };
    /// assert!(WatchCondition::new("seal", ["torn seal"]).matches(&region));
    /// assert!(!WatchCondition::new("seal", ["torn seal"]).with_min_score(0.9).matches(&region));
    /// ```
    pub fn matches(&self, region: &Region) -> bool {
        let label_ok = self.labels.is_empty()
            || region.label.as_deref().is_some_and(|label| {
                self.labels.iter().any(|wanted| wanted.eq_ignore_ascii_case(label))
            });
        label_ok
            && region.score.is_some_and(|score| score >= self.min_score)
            && self.zone.as_ref().is_none_or(|zone| zone.holds(region))
    }

    /// The digest of the condition's canonical JSON, which the `vision.watch.*` audit records
    /// name.
    pub fn digest(&self) -> Digest {
        Digest::of_json(self)
    }
}

/// A `Duration` as whole milliseconds.
mod millis {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        value: &Duration,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(u64::try_from(value.as_millis()).unwrap_or(u64::MAX))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Duration, D::Error> {
        u64::deserialize(deserializer).map(Duration::from_millis)
    }
}
