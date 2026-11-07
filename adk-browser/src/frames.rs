//! A live frame feed from a browser session: a screenshot on an interval, latest frame wins.
//!
//! [`BrowserSession::frames`] starts a feed that a console can relay to a person watching the
//! agent work, for example as server-sent events. It uses only WebDriver screenshots: no
//! remote desktop, no extra port and nothing the page can reach, so it works from a sealed
//! browser too.
//!
//! | Behaviour | Why |
//! |---|---|
//! | Captures only while the session runs; never starts one | Watching must not launch a browser or keep one alive |
//! | Skips a frame identical to the last one published | A still page costs nothing after its first frame |
//! | Skips ticks missed while a screenshot is in flight | A slow page lowers the frame rate instead of queueing |
//! | Keeps only the latest frame | A slow viewer sees the current page, not a backlog |
//! | Holds the session weakly | The feed ends when the session is dropped, and never keeps it alive |

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::session::BrowserSession;

/// The shortest interval [`BrowserSession::frames`] accepts; a shorter one is raised to it.
pub const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(100);

/// One frame of a session's page.
///
/// # Example
///
/// ```rust
/// use adk_browser::Frame;
///
/// fn describe(frame: &Frame) -> String {
///     format!("frame {} ({} bytes of base64 PNG)", frame.seq, frame.png_base64.len())
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Frame {
    /// 1 for the feed's first published frame, rising by one with each published frame.
    pub seq: u64,
    /// When the screenshot was taken.
    pub taken_at: SystemTime,
    /// The page as a base64-encoded PNG, as WebDriver returns it, ready for a data URL.
    pub png_base64: Arc<str>,
}

/// A running frame feed from a [`BrowserSession`], made by [`BrowserSession::frames`].
///
/// Clones share one feed. The feed runs until its last clone is dropped or its session is
/// dropped, whichever comes first. Receivers from [`subscribe`](Self::subscribe) do not keep
/// it running.
///
/// # Example
///
/// ```rust,no_run
/// use std::sync::Arc;
/// use std::time::Duration;
///
/// use adk_browser::{BrowserConfig, BrowserSession};
///
/// # async fn example() -> adk_core::Result<()> {
/// let session = Arc::new(BrowserSession::new(BrowserConfig::new()));
/// session.start().await?;
/// let mut feed = session.frames(Duration::from_secs(1));
/// session.navigate("https://example.com").await?;
/// while let Some(frame) = feed.next().await {
///     // Relay `frame.png_base64` to the console, e.g. as one server-sent event.
///     println!("frame {}", frame.seq);
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct FrameFeed {
    frames: watch::Receiver<Option<Arc<Frame>>>,
    _task: Arc<AbortOnDrop>,
}

impl std::fmt::Debug for FrameFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameFeed")
            .field("latest_seq", &self.frames.borrow().as_ref().map(|frame| frame.seq))
            .finish_non_exhaustive()
    }
}

impl FrameFeed {
    /// The latest frame, or `None` before the first one.
    pub fn latest(&self) -> Option<Arc<Frame>> {
        self.frames.borrow().clone()
    }

    /// Waits for a frame newer than the last one this handle returned, and returns it.
    /// Returns `None` once the feed has ended because its session was dropped.
    pub async fn next(&mut self) -> Option<Arc<Frame>> {
        loop {
            self.frames.changed().await.ok()?;
            if let Some(frame) = self.frames.borrow_and_update().clone() {
                return Some(frame);
            }
        }
    }

    /// A receiver of the latest frame, for a viewer that watches the feed on its own.
    pub fn subscribe(&self) -> watch::Receiver<Option<Arc<Frame>>> {
        self.frames.clone()
    }
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl BrowserSession {
    /// Starts a feed of this session's page: a screenshot every `every` (at least
    /// [`MIN_FRAME_INTERVAL`]), published when it differs from the last frame. See the
    /// [module documentation](crate::frames) for its behaviour.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime, since the feed runs as a task.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use std::time::Duration;
    ///
    /// use adk_browser::{BrowserConfig, BrowserSession};
    ///
    /// # async fn example() {
    /// let session = Arc::new(BrowserSession::new(BrowserConfig::new()));
    /// let feed = session.frames(Duration::from_secs(1));
    /// assert!(feed.latest().is_none(), "nothing is captured before the session starts");
    /// # }
    /// ```
    pub fn frames(self: &Arc<Self>, every: Duration) -> FrameFeed {
        let session = Arc::downgrade(self);
        let (sender, frames) = watch::channel(None);
        let task = tokio::spawn(async move {
            let mut ticks = tokio::time::interval(every.max(MIN_FRAME_INTERVAL));
            ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut seq = 0;
            let mut last: Option<Arc<str>> = None;
            loop {
                ticks.tick().await;
                let Some(session) = session.upgrade() else { break };
                let driver = session.running_driver().await;
                drop(session);
                let Some(driver) = driver else { continue };
                match driver.screenshot_as_png_base64().await {
                    Ok(png) if last.as_deref() == Some(png.as_str()) => {}
                    Ok(png) => {
                        let png: Arc<str> = png.into();
                        last = Some(png.clone());
                        seq += 1;
                        sender.send_replace(Some(Arc::new(Frame {
                            seq,
                            taken_at: SystemTime::now(),
                            png_base64: png,
                        })));
                    }
                    Err(error) => tracing::debug!(%error, "browser frame skipped"),
                }
            }
        });
        FrameFeed { frames, _task: Arc::new(AbortOnDrop(task)) }
    }
}
