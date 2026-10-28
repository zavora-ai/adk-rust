//! JSON lines for CI (spec 3.3 §5.2): the engine's events of one session, one per line, then
//! how the turn ended, as a line and as the exit code.

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Write};
use std::time::{Duration, Instant};

use adk_core::ApprovalState;

use futures::StreamExt;
use serde_json::{Value, json};

use super::{EXIT_BUDGET, EXIT_DONE, EXIT_ERROR, EXIT_WAITING, TerminalSurface};
use crate::engine::{Command, FeedFrame, SurfaceEngine};
use crate::error::SurfaceError;
use crate::model::{AgentRow, AgentStatus, DecisionCard, FeedEntry, Speaker, ThreadItem};
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
    pub(crate) fn done() -> Self {
        Self { outcome: "done", code: EXIT_DONE, text: words::DONE.to_string() }
    }

    pub(crate) fn error(text: impl Into<String>) -> Self {
        Self { outcome: "error", code: EXIT_ERROR, text: text.into() }
    }

    /// A decision waits; `queued` messages of this run were queued behind it and, the run
    /// ending, are not sent: input queued behind a decision waits in this process only.
    pub(crate) fn waiting(heading: &str, queued: usize) -> Self {
        let mut text = format!("{}: {heading}", words::WAITING_FOR_YOU);
        if queued > 0 {
            text.push_str(
                ". Not sent: the message queued behind it; send it again once it is answered",
            );
        }
        Self { outcome: "waitingForYou", code: EXIT_WAITING, text }
    }

    pub(crate) fn budget(spend: &str) -> Self {
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
    /// What the agents last did in the turn was to say something: a turn whose last step is a
    /// tool call or its result did not finish with a reply, whatever ended it.
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
        if entry.seq <= self.baseline {
            return;
        }
        self.began = true;
        for item in &entry.items {
            match item {
                ThreadItem::Message { from: Speaker::Agent, .. } => self.reply = true,
                ThreadItem::Activity { .. } | ThreadItem::Result { .. } => self.reply = false,
                ThreadItem::Message { from: Speaker::You, .. } | ThreadItem::Decision { .. } => {}
                // A drawing is a tool's result, never the agent's reply (spec 3.3 §7.4 rule 5).
                ThreadItem::Drawn { .. }
                | ThreadItem::DrawnRefused { .. }
                | ThreadItem::DrawnSent { .. }
                | ThreadItem::App { .. } => self.reply = false,
            }
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
                    let queued = engine.queued(caller, session).len();
                    return Some(End::waiting(&card.heading, queued));
                }
            }
            Err(error) => return Some(End::error(error.to_string())),
        }
        // Input queued behind a running turn goes once it ends: not settled until then.
        if !engine.queued(caller, session).is_empty() {
            self.quiet = 0;
            return None;
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
        // How the turn ended is read from the session itself, not from a feed that may lag.
        if let Ok(thread) = engine.thread(caller, session).await {
            for entry in &thread {
                self.observe(entry);
            }
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

/// Each agent's version, by id: a change to any field of its definition moves it.
pub(crate) type Versions = BTreeMap<String, u64>;

/// The versions of `caller`'s agents now; none when they cannot be read.
async fn versions(engine: &SurfaceEngine, caller: &Caller) -> Versions {
    engine
        .agent_details(caller)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|detail| (detail.id, detail.version))
        .collect()
}

/// The `agent` lines between two rosters (spec 3.3 §5.2, §5.7: "one line per agent change"):
/// for each agent a person added, `added`, `changed` (any field of its definition: its
/// version moved), `paused`, `resumed` or `removed`, with its roster row as the terminal
/// shows it.
pub(crate) fn agent_changes(
    (was, was_versions): (&[AgentRow], &Versions),
    (now, now_versions): (&[AgentRow], &Versions),
) -> Vec<Value> {
    let added = |rows: &[AgentRow]| -> Vec<AgentRow> {
        rows.iter().filter(|row| row.added).cloned().collect()
    };
    let (was, now) = (added(was), added(now));
    let paused = |row: &AgentRow| row.status == AgentStatus::Paused;
    let mut lines = Vec::new();
    for row in &now {
        let change = match was.iter().find(|before| before.agent.name == row.agent.name) {
            None => Some("added"),
            Some(before) if paused(row) && !paused(before) => Some("paused"),
            Some(before) if paused(before) && !paused(row) => Some("resumed"),
            Some(before)
                if before.agent != row.agent
                    || before.area != row.area
                    || before.mode != row.mode
                    || was_versions.get(&row.agent.name) != now_versions.get(&row.agent.name) =>
            {
                Some("changed")
            }
            Some(_) => None,
        };
        if let Some(change) = change {
            lines.push(json!({ "type": "agent", "change": change, "agent": row }));
        }
    }
    for row in
        was.iter().filter(|before| !now.iter().any(|row| row.agent.name == before.agent.name))
    {
        lines.push(json!({ "type": "agent", "change": "removed", "agent": row }));
    }
    lines
}

/// A `decided` line: a decision of the session answered, with who answered, where and when,
/// as every other surface shows it ("You said yes in Telegram at 07:31", spec 3.3 §8.1).
fn decided_line(out: &mut impl Write, card: &DecisionCard) -> io::Result<()> {
    line(out, &json!({ "type": "decided", "card": card }))
}

/// The decisions of `session` answered before the terminal attached, oldest first.
async fn answered_before(
    engine: &SurfaceEngine,
    caller: &Caller,
    session: &str,
) -> Vec<DecisionCard> {
    let Ok(requests) = engine.source().requests(caller, Some(session), false).await else {
        return Vec::new();
    };
    let mut ended: Vec<_> = requests
        .into_iter()
        .filter(|request| {
            request.scope.session_id == session && request.state != ApprovalState::Pending
        })
        .map(|request| (request.resolved_at.unwrap_or(request.created_at), request.id))
        .collect();
    ended.sort();
    let mut cards = Vec::with_capacity(ended.len());
    for (_, id) in ended {
        if let Ok(card) = engine.decision(caller, &id).await
            && card.decided.is_some()
        {
            cards.push(card);
        }
    }
    cards
}

/// Writes a `decided` line for each decision that was open and no longer is, once.
async fn closed(
    out: &mut impl Write,
    engine: &SurfaceEngine,
    caller: &Caller,
    was_open: &[(String, u64)],
    open: &[(String, u64)],
    told: &mut HashSet<String>,
) -> io::Result<()> {
    for (id, _) in was_open {
        if open.iter().any(|(still, _)| still == id) || told.contains(id) {
            continue;
        }
        if let Ok(card) = engine.decision(caller, id).await
            && card.decided.is_some()
        {
            decided_line(out, &card)?;
            told.insert(id.clone());
        }
    }
    Ok(())
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
    let message = message.filter(|text| !text.trim().is_empty());
    // A message goes to a session of its own unless one is named: never into the thread of a
    // decision this run did not cause. Without one, the run shows where things stand.
    let session = match (&surface.session, &message) {
        (Some(session), _) => session.clone(),
        (None, Some(_)) => uuid::Uuid::new_v4().to_string(),
        (None, None) => surface.initial_session().await,
    };
    let baseline = match engine.thread(caller, &session).await {
        Ok(thread) => thread.len() as u64,
        Err(SurfaceError::NotFound { .. }) => 0,
        Err(error) => return end_line(&mut out, &End::error(error.to_string()), &session),
    };
    let mut settler = Settler::new(baseline);
    // Attached without a message, the terminal says where the session stands: the decisions
    // answered before it came, then its open ones (spec 3.3 §5.2, §8.1 "Decided").
    let mut told = HashSet::new();
    if message.is_none() {
        for card in answered_before(engine, caller, &session).await {
            decided_line(&mut out, &card)?;
            told.insert(card.id.clone());
        }
    }
    // A screen action typed as the message runs as the terminal's caller and ends the run with
    // one `verb` line (spec 3.3 §2 item 7): `{"type":"verb","verb":"snooze","outcome":"done"}`.
    if let Some(text) = &message
        && let Some((name, args)) = super::commands::screen_action(text)
        && let Some(command) = surface.commands.find(name)
    {
        let context = super::CommandContext::new(session.clone());
        let how = caller.origin.kind();
        let (end, value) = match command.run(args, &context).await {
            Ok(said) => (
                End::done(),
                json!({ "type": "verb", "verb": name, "outcome": "done", "how": how, "text": said }),
            ),
            Err(error) => (
                End::error(error.message.clone()),
                json!({ "type": "verb", "verb": name, "outcome": "refused", "how": how, "text": error.message }),
            ),
        };
        line(&mut out, &value)?;
        return end_line(&mut out, &end, &session);
    }
    if let Some(text) = message {
        let command = Command::Send { session_id: session.clone(), text };
        match engine.execute(caller, command).await {
            Ok(outcome) => {
                line(&mut out, &json!({ "type": "command", "outcome": outcome }))?;
                settler.sent();
            }
            Err(error) => return end_line(&mut out, &End::error(error.to_string()), &session),
        }
    }
    // The roster as the run began; each later change of an added agent is one `agent` line.
    let mut roster = engine.agents(caller).await.unwrap_or_default();
    let mut known = versions(engine, caller).await;
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
                            let now: Vec<(String, u64)> =
                                open.iter().map(|card| (card.id.clone(), card.version)).collect();
                            let was = cards.replace(now.clone()).unwrap_or_default();
                            closed(&mut out, engine, caller, &was, &now, &mut told).await?;
                        }
                        FeedFrame::Reset { .. } => last = 0,
                    }
                    frame_line(&mut out, &frame)?;
                    // `drawn`, `drawn.submitted` and `drawn.refused` (spec 3.3 §5.2).
                    if let FeedFrame::Entry { entry } = &frame {
                        for drawn in super::drawn::json_lines(&session, entry) {
                            line(&mut out, &drawn)?;
                        }
                    }
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
                if let Ok(now) = engine.agents(caller).await {
                    let now_known = versions(engine, caller).await;
                    for change in agent_changes((&roster, &known), (&now, &now_known)) {
                        line(&mut out, &change)?;
                    }
                    roster = now;
                    known = now_known;
                }
                if let Some(end) = settler.check(engine, caller, &session).await {
                    // What the feed has not shown yet, so the lines are complete.
                    if let Ok(thread) = engine.thread(caller, &session).await {
                        for entry in thread.into_iter().filter(|entry| entry.seq > last) {
                            let drawn = super::drawn::json_lines(&session, &entry);
                            frame_line(&mut out, &FeedFrame::Entry { entry })?;
                            for drawn in drawn {
                                line(&mut out, &drawn)?;
                            }
                        }
                    }
                    if let Ok(open) = engine.decisions(caller).await {
                        let open: Vec<_> =
                            open.into_iter().filter(|card| card.session_id == session).collect();
                        let signature: Vec<(String, u64)> =
                            open.iter().map(|card| (card.id.clone(), card.version)).collect();
                        let was = cards.clone().unwrap_or_default();
                        closed(&mut out, engine, caller, &was, &signature, &mut told).await?;
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

#[cfg(test)]
mod tests {
    use super::{Versions, agent_changes};
    use crate::model::{AgentRef, AgentRow, AgentStatus, Avatar};

    fn row(name: &str, display: &str, status: AgentStatus, added: bool) -> AgentRow {
        AgentRow {
            agent: AgentRef {
                name: name.into(),
                display_name: display.into(),
                avatar: Avatar { monogram: display[..1].into(), color: "#15803D".into() },
            },
            status,
            status_text: String::new(),
            wake_at: None,
            session_id: None,
            area: None,
            mode: None,
            added,
            can_change: added,
        }
    }

    fn changes(was: &[AgentRow], now: &[AgentRow]) -> Vec<(String, String)> {
        versioned((was, &Versions::new()), (now, &Versions::new()))
    }

    fn versioned(
        was: (&[AgentRow], &Versions),
        now: (&[AgentRow], &Versions),
    ) -> Vec<(String, String)> {
        agent_changes(was, now)
            .into_iter()
            .map(|line| {
                assert_eq!(line["type"], "agent");
                (
                    line["change"].as_str().unwrap().to_string(),
                    line["agent"]["agent"]["name"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    /// One line per change of an agent a person added; the primary agent's work is not one
    /// (spec 3.3 §5.7 "In the terminal").
    #[test]
    fn each_change_of_an_added_agent_is_one_line() {
        let primary = row("adk-rust", "ADK-Rust", AgentStatus::Working, false);
        let ready = row("finance", "Finance", AgentStatus::Ready, true);
        let idle = row("adk-rust", "ADK-Rust", AgentStatus::Done, false);
        assert!(changes(std::slice::from_ref(&primary), std::slice::from_ref(&idle)).is_empty());
        let added = [idle.clone(), ready];
        assert_eq!(
            changes(std::slice::from_ref(&idle), &added),
            [("added".into(), "finance".into())]
        );
        let paused = [idle.clone(), row("finance", "Finance", AgentStatus::Paused, true)];
        assert_eq!(changes(&added, &paused), [("paused".into(), "finance".into())]);
        assert_eq!(changes(&paused, &added), [("resumed".into(), "finance".into())]);
        let renamed = [idle.clone(), row("finance", "Bills", AgentStatus::Ready, true)];
        assert_eq!(changes(&added, &renamed), [("changed".into(), "finance".into())]);
        let working = [idle.clone(), row("finance", "Finance", AgentStatus::Working, true)];
        assert!(changes(&added, &working).is_empty(), "work is not a change of the agent");
        assert_eq!(changes(&added, &[idle]), [("removed".into(), "finance".into())]);
    }

    /// Wave 3 finding 41: a change to a field the roster row does not show — instructions,
    /// what it can use — is one line too, found by its version.
    #[test]
    fn a_change_the_row_does_not_show_is_one_line() {
        let rows = [row("finance", "Finance", AgentStatus::Ready, true)];
        let v1 = Versions::from([("finance".to_string(), 1)]);
        let v2 = Versions::from([("finance".to_string(), 2)]);
        assert!(versioned((&rows, &v1), (&rows, &v1)).is_empty());
        assert_eq!(versioned((&rows, &v1), (&rows, &v2)), [("changed".into(), "finance".into())]);
    }
}
