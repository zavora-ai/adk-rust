//! The camera and the screen as input only while their switches are on (spec 3.3 §5.6).
//!
//! The browser stops a track the moment its switch goes off, but the server does not rely on
//! that: [`VisualInputs`] drops every frame of a source that is off, whatever arrives, and
//! forwards the frames of a source that is on no faster than the provider takes them.
//! Frames are never stored or logged.
//!
//! | Policy | Forwards |
//! |---|---|
//! | [`FramePolicy::Steady`] (Gemini Live) | one frame per interval, by default one a second |
//! | [`FramePolicy::OnChange`] (OpenAI Realtime) | a frame that differs from the last one forwarded, by default at most one every 5 seconds |

use std::time::Duration;

use tokio::time::Instant;

use super::wire::VisualSource;

/// How often a provider takes a camera or screen frame (spec 3.3 §5.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePolicy {
    /// Every frame, at most one per `interval`: Gemini Live takes about one frame a second.
    Steady {
        /// The least time between two forwarded frames of one source.
        interval: Duration,
    },
    /// Only a frame that differs from the last one forwarded, at most one per `interval`:
    /// the OpenAI Realtime API reads an image as part of the conversation, so an unchanged
    /// screen is not sent again.
    OnChange {
        /// The least time between two forwarded frames of one source.
        interval: Duration,
    },
}

impl FramePolicy {
    /// Gemini Live: one frame a second.
    pub const GEMINI: Self = Self::Steady { interval: Duration::from_secs(1) };
    /// The OpenAI Realtime API: on change, at most one frame every 5 seconds.
    pub const OPENAI: Self = Self::OnChange { interval: Duration::from_secs(5) };

    fn interval(self) -> Duration {
        match self {
            Self::Steady { interval } | Self::OnChange { interval } => interval,
        }
    }
}

/// One source's switch and what was last forwarded from it.
#[derive(Debug, Clone, Default)]
struct Switch {
    on: bool,
    /// When the last frame was forwarded.
    last_at: Option<Instant>,
    /// A digest of the last frame forwarded, for [`FramePolicy::OnChange`].
    last_frame: Option<u64>,
}

/// The gate every camera and screen frame passes before it reaches the provider
/// (spec 3.3 §5.6).
///
/// # Example
///
/// ```rust
/// use adk_surface::voice::{FramePolicy, VisualInputs, VisualSource};
/// use tokio::time::Instant;
///
/// let mut gate = VisualInputs::new(FramePolicy::GEMINI);
/// let now = Instant::now();
/// assert!(!gate.offer(VisualSource::Screen, b"frame", now), "off: dropped");
/// gate.switch(VisualSource::Screen, true);
/// assert!(gate.offer(VisualSource::Screen, b"frame", now));
/// gate.switch(VisualSource::Screen, false);
/// assert!(!gate.offer(VisualSource::Screen, b"frame", now + std::time::Duration::from_secs(5)));
/// ```
#[derive(Debug, Clone)]
pub struct VisualInputs {
    policy: FramePolicy,
    camera: Switch,
    screen: Switch,
}

impl VisualInputs {
    /// Both switches off.
    pub fn new(policy: FramePolicy) -> Self {
        Self { policy, camera: Switch::default(), screen: Switch::default() }
    }

    fn of(&mut self, source: VisualSource) -> &mut Switch {
        match source {
            VisualSource::Camera => &mut self.camera,
            VisualSource::Screen => &mut self.screen,
        }
    }

    /// Turns `source` on or off. Off forgets the last frame, so the first frame after the next
    /// on is new.
    pub fn switch(&mut self, source: VisualSource, on: bool) {
        let switch = self.of(source);
        switch.on = on;
        if !on {
            switch.last_frame = None;
        }
    }

    /// Whether `source` is on.
    pub fn is_on(&self, source: VisualSource) -> bool {
        match source {
            VisualSource::Camera => self.camera.on,
            VisualSource::Screen => self.screen.on,
        }
    }

    /// Whether `frame`, received from `source` at `at`, goes to the provider: never while the
    /// source is off, and no faster than the policy allows.
    pub fn offer(&mut self, source: VisualSource, frame: &[u8], at: Instant) -> bool {
        let policy = self.policy;
        let switch = self.of(source);
        if !switch.on {
            return false;
        }
        if switch.last_at.is_some_and(|last| at.saturating_duration_since(last) < policy.interval())
        {
            return false;
        }
        let digest = fingerprint(frame);
        if matches!(policy, FramePolicy::OnChange { .. }) && switch.last_frame == Some(digest) {
            return false;
        }
        switch.last_at = Some(at);
        switch.last_frame = Some(digest);
        true
    }
}

/// A cheap digest of a frame, to tell an unchanged screen from a new one. Not a security
/// boundary: a collision only skips one frame.
fn fingerprint(frame: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    frame.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn on_change_skips_an_unchanged_frame() {
        let mut gate = VisualInputs::new(FramePolicy::OPENAI);
        let start = Instant::now();
        gate.switch(VisualSource::Screen, true);
        assert!(gate.offer(VisualSource::Screen, b"a", start));
        assert!(!gate.offer(VisualSource::Screen, b"b", start + Duration::from_secs(1)));
        assert!(!gate.offer(VisualSource::Screen, b"a", start + Duration::from_secs(6)));
        assert!(gate.offer(VisualSource::Screen, b"b", start + Duration::from_secs(6)));
    }

    #[test]
    fn the_sources_are_independent() {
        let mut gate = VisualInputs::new(FramePolicy::GEMINI);
        let start = Instant::now();
        gate.switch(VisualSource::Camera, true);
        assert!(gate.offer(VisualSource::Camera, b"a", start));
        assert!(!gate.offer(VisualSource::Screen, b"a", start));
        gate.switch(VisualSource::Screen, true);
        assert!(gate.offer(VisualSource::Screen, b"a", start));
        assert!(!gate.offer(VisualSource::Camera, b"b", start + Duration::from_millis(500)));
    }
}
