//! JSON lines for CI (spec 3.3 §5.2): the engine's events of one session, one per line, then
//! how the turn ended, as a line and as the exit code.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{Value, json};

use super::{EXIT_BUDGET, EXIT_DONE, EXIT_ERROR, EXIT_WAITING, TerminalSurface};
use crate::engine::{Command, FeedFrame, SurfaceEngine};
use crate::error::SurfaceError;
use crate::model::{AgentStatus, FeedEntry, Speaker, ThreadItem};
use crate::origin::Caller;
use crate::words;

/// How long a sent message may wait to start before the run counts as failed.
const START_TIMEOUT: Duration = Duration::from_secs(120);

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct End {
    /// `done`, `error`, `waitingForYou` or `budgetReached`.
    pub outcome: &'static str,
    /// The exit code.
    pub code: i32,
    /// What happened, in the engine's words.
    pub text: String,
}

impl End {
    fn done() -> Self {
        Self { outcome: "done", code: EXIT_DONE, text: words::DONE.to_string() }
    }

    fn error(text: impl Into<String>) -> Self {
        Self { outcome: "error", code: EXIT_ERROR, text: text.into() }
    }

    fn waiting(heading: &str) -> Self {
        Self {
            outcome: "waitingForYou",
            code: EXIT_WAITING,
            text: format!("{}: {heading}", words::WAITING_FOR_YOU),
        }
    }

    fn budget(spend: &str) -> Self {
        Self {
            outcome: "budgetReached",
            code: EXIT_BUDGET,
            text: format!("{}: {spend}", words::DAILY_SPENDING_LIMIT),
        }
    }
}

/// Tells when a turn has settled: nothing runs in the session, and it either waits for a
/// decision, failed, reached the daily spending limit, or finished with a reply.
#[derive(Debug)]
pub(crate) struct Settler {
    baseline: u64,
    sent: bool,
    reply: bool,
    /// The session recorded something after the baseline: the turn began.
    began: bool,
    busy_seen: bool,
    quiet: u8,
    since: Instant,
}

impl Settler {
    /// A settler for a session whose store held `baseline` events before the run.
    pub(crate) fn new(baseline: u64) -> Self {
        Self {
            baseline,
            sent: false,
            reply: false,
            began: false,
            busy_seen: false,
            quiet: 0,
            since: Instant::now(),
        }
    }

    /// A message was sent: the run waits for its turn.
    pub(crate) fn sent(&mut self) {
        if !self.sent {
            self.since = Instant::now();
        }
        self.sent = true;
        self.quiet = 0;
    }

    /// An entry of the session's feed.
    pub(crate) fn observe(&mut self, entry: &FeedEntry) {
        if entry.seq > self.baseline {
            self.began = true;
        }
        if entry.seq > self.baseline
            && entry
                .items
                .iter()
                .any(|item| !matches!(item, ThreadItem::Message { from: Speaker::You, .. }))
        {
            self.reply = true;
        }
    }

    /// How the run ended, once it has.
    pub(crate) async fn check(
        &mut self,
        engine: &SurfaceEngine,
        caller: &Caller,
        session: &str,
    ) -> Option<End> {
        let status = match engine.status(caller, Some(session)).await {
            Ok(status) => Some(status),
            Err(_) => engine.status(caller, None).await.ok(),
        };
        if let Some(status) = &status
            && status.stopped
        {
            return Some(End::error(
                status.stopped_text.clone().unwrap_or_else(|| words::ALL_AGENTS_STOPPED.into()),
            ));
        }
        match engine.source().busy(caller.user_id(), session).await {
            Ok(true) => {
                self.busy_seen = true;
                self.quiet = 0;
                return None;
            }
            Ok(false) => {}
            Err(error) => return Some(End::error(error.to_string())),
        }
        match engine.decisions(caller).await {
            Ok(cards) => {
                if let Some(card) = cards.iter().find(|card| card.session_id == session) {
                    return Some(End::waiting(&card.heading));
                }
            }
            Err(error) => return Some(End::error(error.to_string())),
        }
        let spend = status.as_ref().and_then(|status| status.spend.clone());
        let budget = spend.as_ref().filter(|spend| {
            spend.limit_micro_usd.is_some_and(|limit| spend.spent_micro_usd >= limit)
        });
        if !self.sent {
            return Some(match budget {
                Some(spend) => End::budget(&spend.text),
                None => End::done(),
            });
        }
        if !self.busy_seen && !self.began {
            if let Some(spend) = budget {
                return Some(End::budget(&spend.text));
            }
            if self.since.elapsed() > START_TIMEOUT {
                return Some(End::error("The turn did not start."));
            }
            return None;
        }
        // Not running on two reads in a row: settled.
        self.quiet += 1;
        if self.quiet < 2 {
            return None;
        }
        let failed = engine.agents(caller).await.ok().is_some_and(|rows| {
            rows.iter().any(|row| {
                row.session_id.as_deref() == Some(session) && row.status == AgentStatus::Failed
            })
        });
        if failed {
            return Some(End::error(words::FAILED));
        }
        if let Some(spend) = budget {
            return Some(End::budget(&spend.text));
        }
        if !self.reply {
            return Some(End::error("The turn ended without a reply."));
        }
        Some(End::done())
    }
}

/// `value` as one JSON line, with nothing a terminal would act on (spec 3.3 §11 S-I9).
pub(crate) fn encode(value: &impl serde::Serialize) -> io::Result<String> {
    let text = serde_json::to_string(value).map_err(io::Error::other)?;
    Ok(super::safe::json_safe(&text).into_owned())
}

fn line(out: &mut impl Write, value: &Value) -> io::Result<()> {
    out.write_all(encode(value)?.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

fn frame_line(out: &mut impl Write, frame: &FeedFrame) -> io::Result<()> {
    line(out, &serde_json::to_value(frame).map_err(io::Error::other)?)
}

fn end_line(out: &mut impl Write, end: &End, session: &str) -> io::Result<i32> {
    line(
        out,
        &json!({
            "type": "end",
            "outcome": end.outcome,
            "exitCode": end.code,
            "sessionId": session,
            "text": end.text,
        }),
    )?;
    Ok(end.code)
}

pub(crate) async fn run<W: Write + Send>(
    surface: TerminalSurface,
    message: Option<String>,
    mut out: W,
) -> io::Result<i32> {
    let engine = &surface.engine;
    let caller = &surface.caller;
    let session = surface.initial_session().await;
    let baseline = match engine.thread(caller, &session).await {
        Ok(thread) => thread.len() as u64,
        Err(SurfaceError::NotFound { .. }) => 0,
        Err(error) => return end_line(&mut out, &End::error(error.to_string()), &session),
    };
    let mut settler = Settler::new(baseline);
    if let Some(text) = message.filter(|text| !text.trim().is_empty()) {
        let command = Command::Send { session_id: session.clone(), text };
        match engine.execute(caller, command).await {
            Ok(outcome) => {
                line(&mut out, &json!({ "type": "command", "outcome": outcome }))?;
                settler.sent();
            }
            Err(error) => return end_line(&mut out, &End::error(error.to_string()), &session),
        }
    }
    let mut last = baseline;
    let mut cards: Option<Vec<(String, u64)>> = None;
    let mut feed = engine.feed(caller.clone(), session.clone(), Some(last));
    let mut tick = tokio::time::interval(surface.poll);
    loop {
        tokio::select! {
            frame = feed.next() => match frame {
                Some(Ok(frame)) => {
                    match &frame {
                        FeedFrame::Entry { entry } => {
                            settler.observe(entry);
                            last = entry.seq;
                        }
                        FeedFrame::Decisions { cards: open } => {
                            cards = Some(open.iter().map(|card| (card.id.clone(), card.version)).collect());
                        }
                        FeedFrame::Reset { .. } => last = 0,
                    }
                    frame_line(&mut out, &frame)?;
                }
                // A new session exists once its turn starts.
                Some(Err(SurfaceError::NotFound { .. })) | None => {
                    tokio::time::sleep(surface.poll).await;
                    feed = engine.feed(caller.clone(), session.clone(), Some(last));
                }
                Some(Err(error)) => {
                    return end_line(&mut out, &End::error(error.to_string()), &session);
                }
            },
            _ = tick.tick() => {
                if let Some(end) = settler.check(engine, caller, &session).await {
                    // What the feed has not shown yet, so the lines are complete.
                    if let Ok(thread) = engine.thread(caller, &session).await {
                        for entry in thread.into_iter().filter(|entry| entry.seq > last) {
                            frame_line(&mut out, &FeedFrame::Entry { entry })?;
                        }
                    }
                    if let Ok(open) = engine.decisions(caller).await {
                        let open: Vec<_> =
                            open.into_iter().filter(|card| card.session_id == session).collect();
                        let signature: Vec<(String, u64)> =
                            open.iter().map(|card| (card.id.clone(), card.version)).collect();
                        if cards.as_ref() != Some(&signature) && !(cards.is_none() && open.is_empty()) {
                            frame_line(&mut out, &FeedFrame::Decisions { cards: open })?;
                        }
                    }
                    return end_line(&mut out, &end, &session);
                }
            }
        }
    }
}
