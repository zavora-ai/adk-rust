//! The kernel's status as the tray icon follows it (spec 3.3 §5.5, §12 phase 5: "the tray
//! changes within one event"), and Stop all from the tray's menu.

use std::time::Duration;

use futures_util::StreamExt as _;
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::{Badge, DesktopError, Result, ServerTarget, TrayStatus};

/// How often the status is read when no event arrives.
const EVERY: Duration = Duration::from_secs(5);

/// What the last read found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// The kernel answered with its status line.
    Status(TrayStatus),
    /// The kernel needs its token (`401`).
    SignIn,
    /// The kernel answered its health but not its status: an error, an answer that does not
    /// parse, or a read slower than 5 s. The kernel is there, so the windows keep its pages,
    /// and [`StatusWatcher::run`] keeps the last read instead.
    Unread,
    /// Nothing answered: the windows show "Can't reach your agents".
    Unreachable,
}

impl Reach {
    /// The badge it shows: unreachable and sign-in override the count, stopped overrides it too.
    pub fn badge(&self) -> Badge {
        match self {
            Self::Status(status) => Badge::from_status(status),
            Self::SignIn => Badge::SignIn,
            Self::Unread | Self::Unreachable => Badge::Unreachable,
        }
    }

    /// The decisions waiting, exactly, for the dock and launcher badge (spec 3.3 §7.1); `None`
    /// when the badge counts none. The tray icon itself stops at `9+`.
    pub fn count(&self) -> Option<u32> {
        match self {
            Self::Status(status) if !status.stopped && status.decisions > 0 => {
                Some(status.decisions)
            }
            Self::Status(_) | Self::SignIn | Self::Unread | Self::Unreachable => None,
        }
    }
}

/// Follows the kernel's status: `GET /api/surface/status?surface=tray` at start, after every
/// frame of `GET /api/kernel/events` and every 5 s, with `GET /api/kernel/health` for whether
/// the kernel is there at all. A `401` from either means the tray needs the kernel's token.
///
/// # Example
///
/// ```no_run
/// use adk_desktop_core::{ServerTarget, StatusWatcher};
///
/// # async fn example() -> adk_desktop_core::Result<()> {
/// let watcher = StatusWatcher::new(ServerTarget::resolve(None, None, None)?)?;
/// watcher.run(|reach| tracing::info!(badge = ?reach.badge(), "status")).await;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct StatusWatcher {
    target: ServerTarget,
    client: Client,
    stream: Client,
    every: Duration,
}

impl StatusWatcher {
    /// A watcher of `target`'s kernel.
    ///
    /// # Errors
    ///
    /// Returns [`DesktopError::Refused`] when no HTTP client can be built (no TLS is needed, so
    /// this does not happen in practice).
    pub fn new(target: ServerTarget) -> Result<Self> {
        Ok(Self {
            client: client(Some(Duration::from_secs(5)))?,
            stream: client(None)?,
            target,
            every: EVERY,
        })
    }

    /// Reads at least every `every` instead of every 5 s.
    #[must_use]
    pub fn every(mut self, every: Duration) -> Self {
        self.every = every;
        self
    }

    /// Reads the kernel's status once.
    pub async fn read(&self) -> Reach {
        match send(authorised(&self.target, self.client.get(self.target.api("/kernel/health"))))
            .await
        {
            Err(_) => return Reach::Unreachable,
            Ok(response) if response.status() == StatusCode::UNAUTHORIZED => return Reach::SignIn,
            Ok(_) => {}
        }
        let status = self.target.api("/surface/status?surface=tray");
        match send(authorised(&self.target, self.client.get(status))).await {
            Ok(response) if response.status() == StatusCode::UNAUTHORIZED => Reach::SignIn,
            Ok(response) if response.status().is_success() => {
                response.json::<TrayStatus>().await.map_or(Reach::Unread, Reach::Status)
            }
            Ok(_) | Err(_) => Reach::Unread,
        }
    }

    /// Reads the status at start, after every kernel event and at least every 5 s, and calls
    /// `seen` each time it differs from the last. A read the kernel did not answer
    /// ([`Reach::Unread`]) keeps the last one, so one slow or failed read changes nothing.
    /// Logs `badge.count` whenever the badge or the count changes. Runs until the future is
    /// dropped.
    pub async fn run(&self, mut seen: impl FnMut(&Reach)) {
        let wake = Notify::new();
        let reads = async {
            let mut last: Option<Reach> = None;
            loop {
                let reach = self.read().await;
                if reach == Reach::Unread && last.is_some() {
                    tracing::debug!("status not read; the last one stays");
                } else if last.as_ref() != Some(&reach) {
                    let badge = reach.badge();
                    let shown = |reach: &Reach| (reach.badge(), reach.count());
                    if last.as_ref().map(shown) != Some((badge, reach.count())) {
                        tracing::info!(
                            badge.count = reach.count().unwrap_or(0),
                            badge.state = badge.icon(),
                            "badge changed"
                        );
                    }
                    seen(&reach);
                    last = Some(reach);
                }
                tokio::select! {
                    () = wake.notified() => {}
                    () = tokio::time::sleep(self.every) => {}
                }
            }
        };
        tokio::join!(reads, self.follow_events(&wake));
    }

    /// Wakes `wake` on every frame of the kernel's event stream, reconnecting when it ends.
    async fn follow_events(&self, wake: &Notify) {
        loop {
            let request = self
                .stream
                .get(self.target.api("/kernel/events"))
                .header("accept", "text/event-stream");
            match send(authorised(&self.target, request)).await {
                Ok(response) if response.status().is_success() => {
                    let mut body = response.bytes_stream();
                    let mut tail = Vec::new();
                    while let Some(Ok(chunk)) = body.next().await {
                        tail.extend_from_slice(&chunk);
                        let frames = frames_in(&mut tail);
                        if frames > 0 {
                            tracing::debug!(events.frames = frames, "kernel event");
                            wake.notify_one();
                        }
                    }
                    // The stream ended: the kernel may have gone, so read now, not at the tick.
                    wake.notify_one();
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Ok(_) | Err(_) => tokio::time::sleep(self.every).await,
            }
        }
    }
}

/// Stops every agent from the tray (spec 3.3 §8.2 "Stop all", §11 S-I10):
/// `POST /api/surface/commands` with the tray surface and a pointer origin. Stop all needs the
/// operator; a loopback kernel admits this computer as the operator, a kernel started with a
/// token needs the tray started with it.
///
/// # Errors
///
/// [`DesktopError::Unreachable`] when the kernel cannot be reached, [`DesktopError::SignIn`]
/// on `401`, and [`DesktopError::Refused`] with the kernel's problem otherwise.
pub async fn stop_all(target: &ServerTarget) -> Result<()> {
    let body = json!({ "surface": "tray", "origin": { "kind": "pointer" }, "command": { "type": "stopAll" } });
    let request =
        client(Some(Duration::from_secs(5)))?.post(target.api("/surface/commands")).json(&body);
    let response = send(authorised(target, request))
        .await
        .map_err(|source| DesktopError::Unreachable { origin: target.page_origin(), source })?;
    let status = response.status();
    if status.is_success() {
        tracing::info!("stop all sent from the tray");
        return Ok(());
    }
    if status == StatusCode::UNAUTHORIZED {
        return Err(DesktopError::SignIn { origin: target.page_origin() });
    }
    let problem: Value = response.json().await.unwrap_or(Value::Null);
    let field = |name: &str| {
        problem
            .pointer(&format!("/error/{name}"))
            .or_else(|| problem.get(name))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    Err(DesktopError::Refused {
        status: status.as_u16(),
        code: field("code"),
        message: field("message").or_else(|| field("detail")).unwrap_or_else(|| status.to_string()),
    })
}

/// Why Stop all from the tray icon's menu did not go through (spec 3.3 §5.5, S-I10). The
/// menu cannot show it, so the shell opens the panel and hands it this; the panel shows it in
/// its banner, as it shows a failure of its own Stop all.
///
/// # Example
///
/// ```
/// use adk_desktop_core::{DesktopError, TrayProblem};
///
/// let refused = DesktopError::Refused {
///     status: 403,
///     code: Some("surface.operator_only".into()),
///     message: "Stopping every agent needs the operator".into(),
/// };
/// assert_eq!(
///     TrayProblem::of(&refused).script(),
///     r#"window.dispatchEvent(new CustomEvent("adk:tray-problem", { detail: {"kind":"refused","message":"Stopping every agent needs the operator"} }));"#
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TrayProblem {
    /// The kernel did not answer: "Can't reach your agents".
    Unreachable,
    /// The kernel asked for its token: "Start the tray with --token".
    SignIn,
    /// The kernel refused, in its own words.
    Refused {
        /// The kernel's problem, such as "Stopping every agent needs the operator".
        message: String,
    },
}

impl TrayProblem {
    /// The page event the panel listens for.
    pub const EVENT: &'static str = "adk:tray-problem";

    /// The problem `error` is to the person.
    pub fn of(error: &DesktopError) -> Self {
        match error {
            DesktopError::Unreachable { .. } => Self::Unreachable,
            DesktopError::SignIn { .. } => Self::SignIn,
            DesktopError::Refused { message, .. } => Self::Refused { message: message.clone() },
            DesktopError::BadServer { .. }
            | DesktopError::NotLoopback { .. }
            | DesktopError::Config { .. } => Self::Refused { message: error.to_string() },
        }
    }

    /// The script that hands it to the panel's page: a `CustomEvent` named [`Self::EVENT`]
    /// whose `detail` is this problem as JSON.
    pub fn script(&self) -> String {
        let event = Value::from(Self::EVENT);
        let detail = serde_json::to_string(self).unwrap_or_else(|_| "{}".to_owned());
        format!("window.dispatchEvent(new CustomEvent({event}, {{ detail: {detail} }}));")
    }
}

fn client(timeout: Option<Duration>) -> Result<Client> {
    // Loopback only: no proxy, and a short connect timeout so a stopped kernel shows at once.
    let builder = Client::builder().no_proxy().connect_timeout(Duration::from_secs(2));
    let builder = match timeout {
        Some(timeout) => builder.timeout(timeout),
        None => builder,
    };
    builder.build().map_err(|error| DesktopError::Refused {
        status: 0,
        code: None,
        message: error.to_string(),
    })
}

fn authorised(target: &ServerTarget, request: RequestBuilder) -> RequestBuilder {
    match target.token() {
        Some(token) => request.bearer_auth(token.expose()),
        None => request,
    }
}

async fn send(request: RequestBuilder) -> std::result::Result<Response, reqwest::Error> {
    request.header("accept", "application/json").send().await
}

/// Counts the complete server-sent event frames at the start of `buffer` and drops them.
fn frames_in(buffer: &mut Vec<u8>) -> usize {
    let mut frames = 0;
    loop {
        let lf = buffer.windows(2).position(|pair| pair == b"\n\n").map(|at| at + 2);
        let crlf = buffer.windows(4).position(|four| four == b"\r\n\r\n").map(|at| at + 4);
        let Some(end) = lf.into_iter().chain(crlf).min() else { return frames };
        // A frame of comments only (a keep-alive) is not an event.
        if buffer[..end]
            .split(|byte| *byte == b'\n')
            .any(|line| !line.is_empty() && line != b"\r" && line[0] != b':')
        {
            frames += 1;
        }
        buffer.drain(..end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_counted_once_and_keep_alives_are_not() {
        let mut buffer =
            b"id: 1\ndata: {}\n\n: keep-alive\n\nid: 2\r\ndata: {}\r\n\r\nid: 3\nda".to_vec();
        assert_eq!(frames_in(&mut buffer), 2);
        assert_eq!(buffer, b"id: 3\nda");
        buffer.extend_from_slice(b"ta: {}\n\n");
        assert_eq!(frames_in(&mut buffer), 1);
        assert!(buffer.is_empty());
    }

    #[test]
    fn unreachable_and_sign_in_override_the_count() {
        let waiting = Reach::Status(TrayStatus { decisions: 2, ..TrayStatus::default() });
        assert_eq!(waiting.badge(), Badge::Count(2));
        assert_eq!(Reach::SignIn.badge(), Badge::SignIn);
        assert_eq!(Reach::Unreachable.badge(), Badge::Unreachable);
    }

    #[test]
    fn a_failed_stop_all_reaches_the_panel_as_the_problem_it_shows() {
        let sign_in = DesktopError::SignIn { origin: "http://127.0.0.1:8080".into() };
        assert_eq!(TrayProblem::of(&sign_in), TrayProblem::SignIn);
        assert_eq!(
            TrayProblem::SignIn.script(),
            r#"window.dispatchEvent(new CustomEvent("adk:tray-problem", { detail: {"kind":"signIn"} }));"#
        );
        assert_eq!(
            TrayProblem::Unreachable.script(),
            r#"window.dispatchEvent(new CustomEvent("adk:tray-problem", { detail: {"kind":"unreachable"} }));"#
        );
        // The kernel's words stay one JSON string.
        let refused = DesktopError::Refused {
            status: 500,
            code: None,
            message: r#"store "approvals" failed"); alert(1); //"#.into(),
        };
        let script = TrayProblem::of(&refused).script();
        assert!(
            script.ends_with(
                r#"{ detail: {"kind":"refused","message":"store \"approvals\" failed\"); alert(1); //"} }));"#
            ),
            "{script}"
        );
    }

    #[test]
    fn the_dock_and_launcher_count_is_exact_past_nine() {
        let status = |decisions, stopped| {
            Reach::Status(TrayStatus { decisions, stopped, ..TrayStatus::default() })
        };
        assert_eq!(status(25, false).badge(), Badge::Many);
        assert_eq!(status(25, false).count(), Some(25));
        assert_eq!(status(10, false).count(), Some(10));
        assert_eq!(status(3, false).count(), Some(3));
        assert_eq!(status(0, false).count(), None);
        assert_eq!(status(4, true).count(), None);
        assert_eq!(
            [Reach::SignIn, Reach::Unread, Reach::Unreachable].map(|r| r.count()),
            [None; 3]
        );
    }
}
