//! Target evidence and value-free accessibility sensitivity contracts.
//!
//! These types bind a proposed action to a *fresh* desktop observation and
//! carry only digests and structured signals — never raw field values — so the
//! wire payload cannot leak sensitive content across the ADK/runtime boundary.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

/// Evidence binding an action to a fresh desktop observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetEvidence {
    /// Platform identifier for the observed target (e.g. `darwin`, `windows`).
    pub platform: String,
    /// Application/bundle identifier of the observed target.
    pub app_id: String,
    /// Process identifier of the observed target, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Platform-specific window identifier, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<Value>,
    /// Digest of the window title (never the raw title).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title_digest: Option<String>,
    /// Display identifier hosting the target, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_id: Option<String>,
    /// Accessibility role of the target element, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Digest of the element label (never the raw label).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label_digest: Option<String>,
    /// Bounding box of the target element, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    /// Identifier of the observation frame this evidence came from.
    pub observation_id: String,
    /// Hash of the screenshot backing this observation, when captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_hash: Option<String>,
    /// Pixels per logical point of the screenshot `screenshot_hash` names: 2.0 for a
    /// 3024×1964 screenshot of a 1512×982-point display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_pixel_ratio: Option<f64>,
    /// Revision of the UI tree backing this observation, when captured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui_tree_revision: Option<String>,
    /// Observation confidence in `0.0..=1.0`.
    pub confidence: f64,
    /// RFC 3339 timestamp of the observation.
    pub captured_at: String,
}

impl TargetEvidence {
    /// Checks that the evidence binds its bounds to its screenshot (spec 3.7 V-I14): a
    /// target with both bounds and a screenshot hash states the screenshot's device pixel
    /// ratio, the bounds carry the same ratio, and the box is finite and not inverted.
    ///
    /// # Errors
    ///
    /// Returns a message naming the violated rule.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_computer_use::{Bounds, TargetEvidence};
    ///
    /// let mut target = TargetEvidence {
    ///     platform: "darwin".into(),
    ///     app_id: "com.example.crm".into(),
    ///     pid: None,
    ///     window_id: None,
    ///     window_title_digest: None,
    ///     display_id: Some("built-in".into()),
    ///     role: None,
    ///     label_digest: None,
    ///     bounds: Some(Bounds::points(960.0, 590.0, 90.0, 40.0, 2.0)),
    ///     observation_id: "observation-1".into(),
    ///     screenshot_hash: Some("sha256:ab12".into()),
    ///     device_pixel_ratio: Some(2.0),
    ///     ui_tree_revision: None,
    ///     confidence: 0.9,
    ///     captured_at: "2026-11-22T10:00:00Z".into(),
    /// };
    /// assert!(target.check_screen_binding().is_ok());
    /// target.device_pixel_ratio = None;
    /// assert!(target.check_screen_binding().is_err());
    /// ```
    pub fn check_screen_binding(&self) -> Result<(), String> {
        let Some(bounds) = &self.bounds else { return Ok(()) };
        bounds.validate()?;
        if self.screenshot_hash.is_none() {
            return Ok(());
        }
        let Some(ratio) = self.device_pixel_ratio else {
            return Err(
                "target bounds from a screenshot must state the screenshot's devicePixelRatio"
                    .into(),
            );
        };
        if !(ratio.is_finite() && ratio > 0.0) {
            return Err(format!("devicePixelRatio must be a positive number, not {ratio}"));
        }
        if (ratio - bounds.device_pixel_ratio).abs() > f64::EPSILON {
            return Err(format!(
                "target bounds are at device pixel ratio {} but the screenshot is at {ratio}",
                bounds.device_pixel_ratio
            ));
        }
        Ok(())
    }
}

/// The coordinate space a [`Bounds`] is in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSpace {
    /// Logical display points, the space a desktop runtime acts in. Bounds without a
    /// stated space are read as points.
    #[default]
    LogicalPoints,
    /// Pixels of the screenshot the evidence's `screenshotHash` names.
    ScreenshotPixels,
}

fn unit_ratio() -> f64 {
    1.0
}

/// Axis-aligned bounding box, with the coordinate space it is in and the device pixel
/// ratio that relates screenshot pixels to logical points.
///
/// # Example
///
/// ```
/// use adk_computer_use::{Bounds, CoordinateSpace};
///
/// // A Save button at (1920, 1180)–(2100, 1260) in a 3024×1964 screenshot of a
/// // 1512×982-point display.
/// let pixels = Bounds::pixels(1920.0, 1180.0, 180.0, 80.0, 2.0);
/// let points = pixels.to_points();
/// assert_eq!(points.space, CoordinateSpace::LogicalPoints);
/// assert_eq!((points.x, points.y, points.width, points.height), (960.0, 590.0, 90.0, 40.0));
/// assert!(points.contains(points.center()));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
    /// The space `x`, `y`, `width` and `height` are in.
    #[serde(default)]
    pub space: CoordinateSpace,
    /// Screenshot pixels per logical point; 1.0 when unstated.
    #[serde(default = "unit_ratio")]
    pub device_pixel_ratio: f64,
}

impl Bounds {
    /// A box in logical points of a display at `device_pixel_ratio`.
    pub fn points(x: f64, y: f64, width: f64, height: f64, device_pixel_ratio: f64) -> Self {
        Self { x, y, width, height, space: CoordinateSpace::LogicalPoints, device_pixel_ratio }
    }

    /// A box in screenshot pixels of a display at `device_pixel_ratio`.
    pub fn pixels(x: f64, y: f64, width: f64, height: f64, device_pixel_ratio: f64) -> Self {
        Self { x, y, width, height, space: CoordinateSpace::ScreenshotPixels, device_pixel_ratio }
    }

    fn ratio(&self) -> f64 {
        if self.device_pixel_ratio.is_finite() && self.device_pixel_ratio > 0.0 {
            self.device_pixel_ratio
        } else {
            1.0
        }
    }

    fn scaled(&self, factor: f64, space: CoordinateSpace) -> Self {
        Self {
            x: self.x * factor,
            y: self.y * factor,
            width: self.width * factor,
            height: self.height * factor,
            space,
            device_pixel_ratio: self.device_pixel_ratio,
        }
    }

    /// The box in logical points.
    pub fn to_points(&self) -> Self {
        match self.space {
            CoordinateSpace::LogicalPoints => self.clone(),
            CoordinateSpace::ScreenshotPixels => {
                self.scaled(1.0 / self.ratio(), CoordinateSpace::LogicalPoints)
            }
        }
    }

    /// The box in screenshot pixels.
    pub fn to_pixels(&self) -> Self {
        match self.space {
            CoordinateSpace::ScreenshotPixels => self.clone(),
            CoordinateSpace::LogicalPoints => {
                self.scaled(self.ratio(), CoordinateSpace::ScreenshotPixels)
            }
        }
    }

    /// The centre, in the box's own space.
    pub fn center(&self) -> [f64; 2] {
        [self.x + self.width / 2.0, self.y + self.height / 2.0]
    }

    /// Whether `point`, in the box's own space, lies inside it (right and bottom edges
    /// exclusive).
    pub fn contains(&self, point: [f64; 2]) -> bool {
        point[0] >= self.x
            && point[1] >= self.y
            && point[0] < self.x + self.width
            && point[1] < self.y + self.height
    }

    fn validate(&self) -> Result<(), String> {
        let values = [self.x, self.y, self.width, self.height, self.device_pixel_ratio];
        if values.iter().any(|value| !value.is_finite()) {
            return Err("target bounds must be finite numbers".into());
        }
        if self.width < 0.0 || self.height < 0.0 {
            return Err("target bounds must not have a negative width or height".into());
        }
        if self.device_pixel_ratio <= 0.0 {
            return Err("target bounds must have a positive devicePixelRatio".into());
        }
        Ok(())
    }
}

/// Conclusion of an accessibility-based target sensitivity check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSensitivityAssessment {
    /// The target holds sensitive content (e.g. a password field).
    Sensitive,
    /// The target is confirmed non-sensitive.
    NonSensitive,
    /// Sensitivity could not be determined.
    Unknown,
}

/// Source of a [`TargetSensitivityEvidence`] assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSensitivitySource {
    /// Derived from platform accessibility APIs.
    Accessibility,
    /// Native sensitivity signals were unavailable.
    Unavailable,
}

/// A single value-free signal contributing to a sensitivity assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetSensitivitySignal {
    /// Element reports a secure accessibility role.
    SecureRole,
    /// Element reports a secure accessibility subrole.
    SecureSubrole,
    /// Element is marked as protected content.
    ProtectedContent,
    /// UI Automation reports the element as a password field.
    UiaIsPassword,
    /// Element label matched a sensitive pattern.
    SensitiveLabel,
    /// Multiple candidate elements matched ambiguously.
    AmbiguousMatch,
    /// The referenced element was not found.
    ElementNotFound,
    /// Inspection raised an error.
    InspectionError,
    /// The field was invalid for inspection.
    InvalidField,
    /// Native sensitivity signals were unavailable.
    NativeSignalUnavailable,
}

/// Value-free native accessibility evidence used for action risk and revalidation.
///
/// Construct with [`TargetSensitivityEvidence::try_new`]; the constructor and the
/// deserializer enforce the same invariants (bounded/unique signals, conclusive
/// assessments require accessibility evidence for a checked field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawTargetSensitivityEvidence", into = "RawTargetSensitivityEvidence")]
pub struct TargetSensitivityEvidence {
    assessment: TargetSensitivityAssessment,
    source: TargetSensitivitySource,
    signals: Vec<TargetSensitivitySignal>,
    fields_checked: u32,
    observed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawTargetSensitivityEvidence {
    assessment: TargetSensitivityAssessment,
    source: TargetSensitivitySource,
    signals: Vec<TargetSensitivitySignal>,
    fields_checked: u32,
    observed_at: String,
}

impl TargetSensitivityEvidence {
    /// Build validated sensitivity evidence.
    ///
    /// # Errors
    ///
    /// Returns a message describing the violated invariant when signals exceed
    /// 10, contain duplicates, `fields_checked` exceeds 100, a conclusive
    /// assessment lacks accessibility evidence for a checked field, a
    /// `Sensitive` assessment has no signals, or `observed_at` is blank.
    ///
    /// # Example
    ///
    /// ```
    /// use adk_computer_use::{
    ///     TargetSensitivityAssessment, TargetSensitivityEvidence, TargetSensitivitySignal,
    ///     TargetSensitivitySource,
    /// };
    ///
    /// let evidence = TargetSensitivityEvidence::try_new(
    ///     TargetSensitivityAssessment::Sensitive,
    ///     TargetSensitivitySource::Accessibility,
    ///     vec![TargetSensitivitySignal::UiaIsPassword],
    ///     1,
    ///     "2026-07-13T12:00:00Z",
    /// )
    /// .unwrap();
    /// assert_eq!(evidence.fields_checked(), 1);
    /// ```
    pub fn try_new(
        assessment: TargetSensitivityAssessment,
        source: TargetSensitivitySource,
        signals: Vec<TargetSensitivitySignal>,
        fields_checked: u32,
        observed_at: impl Into<String>,
    ) -> Result<Self, String> {
        RawTargetSensitivityEvidence {
            assessment,
            source,
            signals,
            fields_checked,
            observed_at: observed_at.into(),
        }
        .try_into()
    }

    /// The conclusion of the sensitivity check.
    pub fn assessment(&self) -> TargetSensitivityAssessment {
        self.assessment
    }

    /// The source of the assessment.
    pub fn source(&self) -> TargetSensitivitySource {
        self.source
    }

    /// The distinct signals that contributed to the assessment.
    pub fn signals(&self) -> &[TargetSensitivitySignal] {
        &self.signals
    }

    /// The number of fields inspected during the check.
    pub fn fields_checked(&self) -> u32 {
        self.fields_checked
    }

    /// RFC 3339 timestamp of the observation.
    pub fn observed_at(&self) -> &str {
        &self.observed_at
    }
}

impl TryFrom<RawTargetSensitivityEvidence> for TargetSensitivityEvidence {
    type Error = String;

    fn try_from(raw: RawTargetSensitivityEvidence) -> Result<Self, Self::Error> {
        if raw.signals.len() > 10 {
            return Err("target sensitivity supports at most 10 signals".into());
        }
        if raw.signals.iter().copied().collect::<HashSet<_>>().len() != raw.signals.len() {
            return Err("target sensitivity signals must be unique".into());
        }
        if raw.fields_checked > 100 {
            return Err("target sensitivity supports at most 100 checked fields".into());
        }
        if matches!(
            raw.assessment,
            TargetSensitivityAssessment::Sensitive | TargetSensitivityAssessment::NonSensitive
        ) && (raw.source != TargetSensitivitySource::Accessibility || raw.fields_checked == 0)
        {
            return Err(
                "conclusive target sensitivity requires accessibility evidence for a checked field"
                    .into(),
            );
        }
        if raw.assessment == TargetSensitivityAssessment::Sensitive && raw.signals.is_empty() {
            return Err("sensitive target evidence requires at least one signal".into());
        }
        if raw.observed_at.trim().is_empty() {
            return Err("target sensitivity observedAt must not be empty".into());
        }
        Ok(Self {
            assessment: raw.assessment,
            source: raw.source,
            signals: raw.signals,
            fields_checked: raw.fields_checked,
            observed_at: raw.observed_at,
        })
    }
}

impl From<TargetSensitivityEvidence> for RawTargetSensitivityEvidence {
    fn from(value: TargetSensitivityEvidence) -> Self {
        Self {
            assessment: value.assessment,
            source: value.source,
            signals: value.signals,
            fields_checked: value.fields_checked,
            observed_at: value.observed_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence(bounds: Option<Bounds>, hash: Option<&str>, ratio: Option<f64>) -> TargetEvidence {
        TargetEvidence {
            platform: "darwin".into(),
            app_id: "com.example.form".into(),
            pid: None,
            window_id: None,
            window_title_digest: None,
            display_id: None,
            role: None,
            label_digest: None,
            bounds,
            observation_id: "observation-1".into(),
            screenshot_hash: hash.map(str::to_string),
            device_pixel_ratio: ratio,
            ui_tree_revision: None,
            confidence: 1.0,
            captured_at: "2026-11-22T10:00:00Z".into(),
        }
    }

    #[test]
    fn screen_binding_requires_a_matching_ratio() {
        let points = Bounds::points(960.0, 590.0, 90.0, 40.0, 2.0);
        assert!(evidence(None, None, None).check_screen_binding().is_ok());
        assert!(evidence(Some(points.clone()), None, None).check_screen_binding().is_ok());
        assert!(
            evidence(Some(points.clone()), Some("sha256:1"), Some(2.0))
                .check_screen_binding()
                .is_ok()
        );
        let missing = evidence(Some(points.clone()), Some("sha256:1"), None);
        assert!(missing.check_screen_binding().unwrap_err().contains("devicePixelRatio"));
        let mismatched = evidence(Some(points), Some("sha256:1"), Some(1.0));
        assert!(mismatched.check_screen_binding().unwrap_err().contains("device pixel ratio 2"));
        let inverted =
            evidence(Some(Bounds::points(0.0, 0.0, -1.0, 4.0, 1.0)), Some("sha256:1"), Some(1.0));
        assert!(inverted.check_screen_binding().is_err());
    }

    #[test]
    fn evidence_without_a_ratio_serializes_as_before() {
        let value = serde_json::to_value(evidence(None, None, None)).unwrap();
        assert!(value.get("devicePixelRatio").is_none());
    }
}
