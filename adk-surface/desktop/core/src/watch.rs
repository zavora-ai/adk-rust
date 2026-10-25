//! The kernel's status as the tray icon follows it (spec 3.3 §5.5, §12 phase 5: "the tray
//! changes within one event"), and Stop all from the tray's menu.

use std::time::Duration;

use futures_util::StreamExt as _;
use reqwest::{Client, RequestBuilder, Response, StatusCode};
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
    /// Nothing answered, or the kernel could not give its status.
    Unreachable,
}

impl Reach {
    /// The badge it shows: unreachable and sign-in override the count, stopped overrides it too.
    pub fn badge(&self) -> Badge {
        match self {
            Self::Status(status) => Badge::from_status(status),
            Self::SignIn => Badge::SignIn,
            Self::Unreachable => Badge::Unreachable,
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
                response.json::<TrayStatus>().await.map_or(Reach::Unreachable, Reach::Status)
            }
            Ok(_) | Err(_) => Reach::Unreachable,
        }
    }

    /// Reads the status at start, after every kernel event and at least every 5 s, and calls
    /// `seen` each time it differs from the last. Logs `badge.count` whenever the badge changes.
    /// Runs until the future is dropped.
    pub async fn run(&self, mut seen: impl FnMut(&Reach)) {
        let wake = Notify::new();
        let reads = async {
            let mut last: Option<Reach> = None;
            loop {
                let reach = self.read().await;
                if last.as_ref() != Some(&reach) {
                    let badge = reach.badge();
                    if last.as_ref().map(Reach::badge) != Some(badge) {
                        tracing::info!(
                            badge.count = badge.count().unwrap_or(0),
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
}
